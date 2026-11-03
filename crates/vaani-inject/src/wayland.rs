//! Text insertion on Wayland compositors with a data-control protocol: the standard ext one
//! (KDE Plasma 6 and newer compositors) or the older wlr one (sway, Hyprland and other
//! wlroots desktops). XWayland's X11 clipboard is not passed on to
//! Wayland apps unless an X window has focus, so Vaani owns the Wayland clipboard itself.
//!
//! Same flow as on X11: save the user's clipboard, offer our text, press paste through the
//! uinput keyboard, watch for the app reading it, then put the old clipboard back. Keys are
//! looked up in the compositor's keymap, so any layout works.
//!
//! When the focused app can't be told, the text goes on the primary selection too and the
//! paste key is Shift+Insert, which pastes in terminals and GUI apps alike.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use xkbcommon::xkb;

use crate::InjectError;

fn w<E: std::fmt::Display>(e: E) -> InjectError {
    InjectError::Wayland(e.to_string())
}

/// mime type -> bytes
type Offer = BTreeMap<String, Vec<u8>>;

/// Text types we offer, so every toolkit finds one it asks for.
const TEXT_TYPES: &[&str] = &[
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "STRING",
    "TEXT",
];

/// Clipboard items bigger than this are not saved (huge images would stall the paste).
const SAVE_LIMIT: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Sel {
    Clipboard,
    Primary,
}

fn text_offer(text: &str) -> Offer {
    TEXT_TYPES
        .iter()
        .map(|t| ((*t).to_string(), text.as_bytes().to_vec()))
        .collect()
}

fn poll_in(fd: std::os::fd::BorrowedFd<'_>) -> bool {
    use rustix::event::{PollFd, PollFlags, Timespec};
    let mut p = [PollFd::new(&fd, PollFlags::IN)];
    let zero = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    rustix::event::poll(&mut p, Some(&zero)).is_ok_and(|n| n > 0)
}

/// The focused app's id, lowercased, from the compositor's own IPC (sway, Hyprland).
/// Empty when it can't be told.
pub(crate) fn focused_app() -> String {
    sway_focused()
        .or_else(hyprland_focused)
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn ipc(path: &str, request: &[u8]) -> Option<Vec<u8>> {
    let mut s = UnixStream::connect(path).ok()?;
    s.set_read_timeout(Some(Duration::from_millis(300))).ok()?;
    s.write_all(request).ok()?;
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out);
    Some(out)
}

fn sway_focused() -> Option<String> {
    let sock = std::env::var("SWAYSOCK").ok()?;
    // i3-ipc header: magic, payload length, message type 4 (GET_TREE)
    let mut req = b"i3-ipc".to_vec();
    req.extend_from_slice(&0u32.to_ne_bytes());
    req.extend_from_slice(&4u32.to_ne_bytes());
    let mut s = UnixStream::connect(sock).ok()?;
    s.set_read_timeout(Some(Duration::from_millis(300))).ok()?;
    s.write_all(&req).ok()?;
    let mut head = [0u8; 14];
    s.read_exact(&mut head).ok()?;
    let len = u32::from_ne_bytes(head[6..10].try_into().ok()?) as usize;
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).ok()?;
    let tree: serde_json::Value = serde_json::from_slice(&body).ok()?;
    find_focused(&tree)
}

fn find_focused(node: &serde_json::Value) -> Option<String> {
    if node.get("focused").and_then(|f| f.as_bool()) == Some(true) {
        let id = node.get("app_id").and_then(|v| v.as_str()).or_else(|| {
            node.pointer("/window_properties/class")
                .and_then(|v| v.as_str())
        })?;
        return Some(id.to_string());
    }
    for key in ["nodes", "floating_nodes"] {
        if let Some(children) = node.get(key).and_then(|v| v.as_array()) {
            if let Some(found) = children.iter().find_map(find_focused) {
                return Some(found);
            }
        }
    }
    None
}

fn hyprland_focused() -> Option<String> {
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    let dir = std::env::var("XDG_RUNTIME_DIR").ok()?;
    let out = ipc(&format!("{dir}/hypr/{sig}/.socket.sock"), b"j/activewindow")?;
    let v: serde_json::Value = serde_json::from_slice(&out).ok()?;
    v.get("class")
        .and_then(|c| c.as_str())
        .filter(|c| !c.is_empty())
        .map(str::to_string)
}

fn read_keymap(fd: OwnedFd, size: usize) -> Option<xkb::Keymap> {
    let mut text = Vec::with_capacity(size);
    std::fs::File::from(fd).read_to_end(&mut text).ok()?;
    while text.last() == Some(&0) {
        text.pop();
    }
    let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    xkb::Keymap::new_from_string(
        &ctx,
        String::from_utf8(text).ok()?,
        xkb::KEYMAP_FORMAT_TEXT_V1,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
}

/// The standard data-control protocol (KDE Plasma 6, newer wlroots and others).
pub(crate) mod ext {
    use wayland_protocols::ext::data_control::v1::client::{
        ext_data_control_device_v1 as dc_device,
        ext_data_control_device_v1::ExtDataControlDeviceV1 as Device,
        ext_data_control_manager_v1::ExtDataControlManagerV1 as Manager,
        ext_data_control_offer_v1 as dc_offer,
        ext_data_control_offer_v1::ExtDataControlOfferV1 as DcOffer,
        ext_data_control_source_v1 as dc_source,
        ext_data_control_source_v1::ExtDataControlSourceV1 as Source,
    };
    const MAX_VERSION: u32 = 1;
    include!("data_control.rs");
}

/// The wlr data-control protocol (sway, Hyprland and older KDE).
pub(crate) mod wlr {
    use wayland_protocols_wlr::data_control::v1::client::{
        zwlr_data_control_device_v1 as dc_device,
        zwlr_data_control_device_v1::ZwlrDataControlDeviceV1 as Device,
        zwlr_data_control_manager_v1::ZwlrDataControlManagerV1 as Manager,
        zwlr_data_control_offer_v1 as dc_offer,
        zwlr_data_control_offer_v1::ZwlrDataControlOfferV1 as DcOffer,
        zwlr_data_control_source_v1 as dc_source,
        zwlr_data_control_source_v1::ZwlrDataControlSourceV1 as Source,
    };
    const MAX_VERSION: u32 = 2;
    include!("data_control.rs");
}

/// Whether this compositor offers either data-control protocol.
pub(crate) fn available() -> bool {
    ext::available() || wlr::available()
}
