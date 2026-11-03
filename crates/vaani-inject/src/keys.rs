//! Synthetic key presses. On X11 they go through XTest. On Wayland XTest only reaches
//! XWayland windows, so keys go through a uinput virtual keyboard instead, which the
//! compositor treats like a real one. systemd gives the logged-in user access to
//! /dev/uinput on most distros; `vaani doctor` says when it doesn't.
//!
//! Key codes are X keycodes either way: XWayland's keycodes are evdev codes plus 8, and its
//! keymap mirrors the compositor's, so the same lookup finds the right key on both.

use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode};
use x11rb::CURRENT_TIME;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::Window;
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use crate::InjectError;

pub(crate) enum Keys {
    XTest,
    Uinput(VirtualDevice),
    /// Keys are only logged (VAANI_INJECT_DRY_RUN=1, for testing against a nested desktop
    /// without pressing keys in the real one).
    Dry,
}

impl Keys {
    /// A uinput keyboard that can press every standard key.
    pub(crate) fn uinput() -> Result<Self, InjectError> {
        if std::env::var_os("VAANI_INJECT_DRY_RUN").is_some() {
            return Ok(Self::Dry);
        }
        let mut keys = AttributeSet::<KeyCode>::new();
        for code in 1..=248 {
            keys.insert(KeyCode::new(code));
        }
        let dev = VirtualDevice::builder()
            .and_then(|b| b.name("Vaani virtual keyboard").with_keys(&keys))
            .and_then(|b| b.build())
            .map_err(|e| InjectError::Uinput(e.to_string()))?;
        // The compositor needs a moment to notice a new keyboard; it is made once at startup.
        Ok(Self::Uinput(dev))
    }

    /// Whether characters missing from the layout can be typed (XTest remaps a spare key;
    /// a uinput keyboard can't change the compositor's keymap).
    /// Keys go through the virtual keyboard (a Wayland session).
    pub(crate) fn is_uinput(&self) -> bool {
        !matches!(self, Self::XTest)
    }

    pub(crate) fn can_remap(&self) -> bool {
        matches!(self, Self::XTest)
    }

    pub(crate) fn key(
        &mut self,
        conn: &RustConnection,
        root: Window,
        code: u8,
        down: bool,
    ) -> Result<(), InjectError> {
        match self {
            Self::XTest => {
                let kind = if down { 2 } else { 3 };
                conn.xtest_fake_input(kind, code, CURRENT_TIME, root, 0, 0, 0)
                    .map_err(crate::x)?;
                Ok(())
            }
            _ => self.evdev(u16::from(code.saturating_sub(8)), down),
        }
    }

    /// Press or release an evdev key (uinput only).
    pub(crate) fn evdev(&mut self, code: u16, down: bool) -> Result<(), InjectError> {
        match self {
            Self::XTest => Err(InjectError::Uinput("no virtual keyboard".into())),
            Self::Dry => {
                tracing::info!(code, down, "key (dry run)");
                Ok(())
            }
            Self::Uinput(dev) => dev
                .emit(&[InputEvent::new(EventType::KEY.0, code, i32::from(down))])
                .map_err(|e| InjectError::Uinput(e.to_string())),
        }
    }

    /// Send queued key events now.
    pub(crate) fn flush(&self, conn: &RustConnection) -> Result<(), InjectError> {
        if matches!(self, Self::XTest) {
            conn.flush().map_err(crate::x)?;
        }
        Ok(())
    }
}
