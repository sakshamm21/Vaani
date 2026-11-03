//! Global hotkey detection.
//!
//! On Wayland the keyboards are read through evdev when /dev/input is readable, and
//! otherwise the GlobalShortcuts portal is used (see those modules).
//!
//! X11 backend: XInput2 raw key events on the root window. They arrive from every keyboard
//! without grabbing the key, so the key keeps working in other apps, and they report key-up,
//! which push-to-talk needs. Auto-repeat presses are ignored. Esc while dictating cancels.

use std::os::fd::AsFd;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossbeam_channel::Sender;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xinput::{self, ConnectionExt as _, KeyEventFlags};
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

mod evdev;
mod portal;

#[derive(Debug, thiserror::Error)]
pub enum HotkeyError {
    #[error(
        "cannot read the keyboards in /dev/input ({0} not permitted); add yourself to the `input` group"
    )]
    NoInputAccess(usize),
    #[error("global shortcuts portal: {0}")]
    Portal(String),
    #[error("{0}")]
    Io(String),
    #[error("cannot connect to the X server (is this an X11 session?): {0}")]
    Connect(String),
    #[error("XInput2 is not available: {0}")]
    NoXInput(String),
    #[error("unknown key name `{0}`")]
    UnknownKey(String),
    #[error("key `{0}` is not on this keyboard layout")]
    NotOnKeyboard(String),
    #[error("X11 error: {0}")]
    X11(String),
}

fn x<E: std::fmt::Display>(e: E) -> HotkeyError {
    HotkeyError::X11(e.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    /// The hotkey went down (auto-repeat filtered out).
    Down,
    /// The hotkey came back up.
    Up,
    /// Esc was pressed (the app decides whether it means "cancel").
    Escape,
    /// The command-mode key went down.
    CommandDown,
    /// The command-mode key came back up.
    CommandUp,
}

/// The keys a listener watches: the dictation key and an optional command-mode key
/// (empty when off).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Watch {
    pub(crate) key: String,
    pub(crate) command: String,
}

/// Keys a person can pick, as (name shown and stored in config, X keysym).
pub const KEYS: &[(&str, u32)] = &[
    ("RightCtrl", 0xffe4),
    ("RightAlt", 0xffea),
    ("RightShift", 0xffe2),
    ("RightSuper", 0xffec),
    ("LeftCtrl", 0xffe3),
    ("LeftAlt", 0xffe9),
    ("Super", 0xffeb),
    ("Menu", 0xff67),
    ("Insert", 0xff63),
    ("Pause", 0xff13),
    ("ScrollLock", 0xff14),
    ("F1", 0xffbe),
    ("F2", 0xffbf),
    ("F3", 0xffc0),
    ("F4", 0xffc1),
    ("F5", 0xffc2),
    ("F6", 0xffc3),
    ("F7", 0xffc4),
    ("F8", 0xffc5),
    ("F9", 0xffc6),
    ("F10", 0xffc7),
    ("F11", 0xffc8),
    ("F12", 0xffc9),
];

pub fn keysym_for(name: &str) -> Option<u32> {
    KEYS.iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, s)| *s)
}

pub fn name_for_keysym(sym: u32) -> Option<&'static str> {
    KEYS.iter().find(|(_, s)| *s == sym).map(|(n, _)| *n)
}

const ESCAPE: u32 = 0xff1b;

struct Keymap {
    min: u8,
    per: usize,
    syms: Vec<u32>,
}

impl Keymap {
    fn load(conn: &RustConnection) -> Result<Self, HotkeyError> {
        let setup = conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let map = conn
            .get_keyboard_mapping(min, max - min + 1)
            .map_err(x)?
            .reply()
            .map_err(x)?;
        Ok(Self {
            min,
            per: usize::from(map.keysyms_per_keycode).max(1),
            syms: map.keysyms,
        })
    }
    fn codes(&self, sym: u32) -> Vec<u32> {
        self.syms
            .chunks(self.per)
            .enumerate()
            .filter(|(_, s)| s.contains(&sym))
            .map(|(i, _)| u32::from(self.min) + i as u32)
            .collect()
    }
    fn sym(&self, code: u32) -> Option<u32> {
        let i = (code.checked_sub(u32::from(self.min))?) as usize;
        self.syms.get(i * self.per).copied().filter(|s| *s != 0)
    }
}

fn connect() -> Result<(RustConnection, u32), HotkeyError> {
    let (conn, screen) = x11rb::connect(None).map_err(|e| HotkeyError::Connect(e.to_string()))?;
    let root = conn.setup().roots[screen].root;
    let v = conn
        .xinput_xi_query_version(2, 2)
        .map_err(x)?
        .reply()
        .map_err(|e| HotkeyError::NoXInput(e.to_string()))?;
    if v.major_version < 2 {
        return Err(HotkeyError::NoXInput(format!(
            "{}.{}",
            v.major_version, v.minor_version
        )));
    }
    conn.xinput_xi_select_events(
        root,
        &[xinput::EventMask {
            deviceid: 1, // all master devices
            mask: vec![xinput::XIEventMask::RAW_KEY_PRESS | xinput::XIEventMask::RAW_KEY_RELEASE],
        }],
    )
    .map_err(x)?
    .check()
    .map_err(x)?;
    conn.flush().map_err(x)?;
    Ok((conn, root))
}

/// A running hotkey listener. Change the key with [`Listener::set_key`]; drop to stop.
pub struct Listener {
    key: Arc<Mutex<Watch>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

/// How the hotkey is detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// XInput2 raw key events (X11 sessions).
    X11,
    /// Reading /dev/input directly (Wayland, when permitted).
    Evdev,
    /// The desktop's GlobalShortcuts portal (Wayland fallback).
    Portal,
}

impl Backend {
    pub fn label(self) -> &'static str {
        match self {
            Self::X11 => "X11 (XInput2)",
            Self::Evdev => "evdev (/dev/input)",
            Self::Portal => "GlobalShortcuts portal",
        }
    }
}

fn wayland_session() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v == "wayland")
}

/// The backend [`listen`] would use right now.
pub fn backend() -> Backend {
    if !wayland_session() {
        Backend::X11
    } else if evdev::available() {
        Backend::Evdev
    } else {
        Backend::Portal
    }
}

/// Whether the GlobalShortcuts portal answers (for `vaani doctor`).
pub fn portal_available() -> bool {
    portal::available()
}

/// Start listening for `key`, with `backend` or else the best one for this session.
/// Events go to `tx`.
pub fn listen(
    key: &str,
    command_key: &str,
    backend: Option<Backend>,
    tx: Sender<HotkeyEvent>,
) -> Result<(Listener, Backend), HotkeyError> {
    if !command_key.is_empty() {
        keysym_for(command_key).ok_or_else(|| HotkeyError::UnknownKey(command_key.into()))?;
    }
    let watch = Watch {
        key: key.to_string(),
        command: command_key.to_string(),
    };
    let b = backend.unwrap_or_else(self::backend);
    let l = match b {
        Backend::X11 => listen_x11(watch, tx)?,
        Backend::Evdev => evdev::listen(watch, tx)?,
        Backend::Portal => portal::listen(watch, tx)?,
    };
    Ok((l, b))
}

/// Wait for the next key press from a pickable key and return its name (for the settings
/// "press a key" button). Returns None after `timeout`.
pub fn capture_next_key(timeout: Duration) -> Result<Option<&'static str>, HotkeyError> {
    match backend() {
        Backend::X11 => capture_x11(timeout),
        Backend::Evdev => evdev::capture_next_key(timeout),
        Backend::Portal => Err(HotkeyError::Portal(
            "change the shortcut in your desktop's keyboard settings".into(),
        )),
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Listener {
    pub fn set_key(&self, name: &str) -> Result<(), HotkeyError> {
        keysym_for(name).ok_or_else(|| HotkeyError::UnknownKey(name.into()))?;
        if let Ok(mut k) = self.key.lock() {
            k.key = name.to_string();
        }
        Ok(())
    }

    /// Set the command-mode key; empty turns it off.
    pub fn set_command_key(&self, name: &str) -> Result<(), HotkeyError> {
        if !name.is_empty() {
            keysym_for(name).ok_or_else(|| HotkeyError::UnknownKey(name.into()))?;
        }
        if let Ok(mut k) = self.key.lock() {
            k.command = name.to_string();
        }
        Ok(())
    }
}

/// Start listening for `key` on X11. Events go to `tx`.
fn listen_x11(watch: Watch, tx: Sender<HotkeyEvent>) -> Result<Listener, HotkeyError> {
    let key = &watch.key;
    keysym_for(key).ok_or_else(|| HotkeyError::UnknownKey(key.into()))?;
    let (conn, _root) = connect()?;
    let keymap = Keymap::load(&conn)?;
    if keymap.codes(keysym_for(key).unwrap_or(0)).is_empty() {
        return Err(HotkeyError::NotOnKeyboard(key.into()));
    }
    let key = Arc::new(Mutex::new(watch));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let listener = Listener {
        key: key.clone(),
        stop: stop.clone(),
    };
    thread::Builder::new()
        .name("hotkey".into())
        .spawn(move || {
            let esc = keymap.codes(ESCAPE);
            let mut current = Watch::default();
            let mut hot: Vec<u32> = Vec::new();
            let mut cmd: Vec<u32> = Vec::new();
            let mut down = false;
            let mut cmd_down = false;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                if let Ok(k) = key.lock() {
                    if *k != current {
                        current = k.clone();
                        hot = keymap.codes(keysym_for(&current.key).unwrap_or(0));
                        cmd = keysym_for(&current.command)
                            .map(|s| keymap.codes(s))
                            .unwrap_or_default();
                        down = false;
                        cmd_down = false;
                        tracing::info!(key = %current.key, command = %current.command, "hotkey set");
                    }
                }
                let event = match conn.poll_for_event() {
                    Ok(Some(e)) => e,
                    Ok(None) => {
                        // Sleep until X has something for us; wake now and then to notice
                        // a changed key or a stop request.
                        wait_readable(&[conn.stream().as_fd()], 250);
                        continue;
                    }
                    Err(e) => {
                        tracing::error!("hotkey listener lost the X connection: {e}");
                        return;
                    }
                };
                let send = |ev| {
                    let _ = tx.send(ev);
                };
                match event {
                    Event::XinputRawKeyPress(e) if hot.contains(&e.detail) => {
                        if !down && !e.flags.contains(KeyEventFlags::KEY_REPEAT) {
                            down = true;
                            send(HotkeyEvent::Down);
                        }
                    }
                    Event::XinputRawKeyRelease(e) if hot.contains(&e.detail) => {
                        if down {
                            down = false;
                            send(HotkeyEvent::Up);
                        }
                    }
                    Event::XinputRawKeyPress(e) if cmd.contains(&e.detail) => {
                        if !cmd_down && !e.flags.contains(KeyEventFlags::KEY_REPEAT) {
                            cmd_down = true;
                            send(HotkeyEvent::CommandDown);
                        }
                    }
                    Event::XinputRawKeyRelease(e) if cmd.contains(&e.detail) => {
                        if cmd_down {
                            cmd_down = false;
                            send(HotkeyEvent::CommandUp);
                        }
                    }
                    Event::XinputRawKeyPress(e) if esc.contains(&e.detail) => {
                        send(HotkeyEvent::Escape)
                    }
                    _ => {}
                }
            }
        })
        .map_err(x)?;
    Ok(listener)
}

fn capture_x11(timeout: Duration) -> Result<Option<&'static str>, HotkeyError> {
    let (conn, _) = connect()?;
    let keymap = Keymap::load(&conn)?;
    let end = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < end {
        match conn.poll_for_event().map_err(x)? {
            Some(Event::XinputRawKeyPress(e)) => {
                if let Some(name) = keymap.sym(e.detail).and_then(name_for_keysym) {
                    return Ok(Some(name));
                }
            }
            Some(_) => {}
            None => wait_readable(&[conn.stream().as_fd()], 50),
        }
    }
    Ok(None)
}

/// Block until any of `fds` is readable or `timeout_ms` passes (no busy polling).
fn wait_readable(fds: &[std::os::fd::BorrowedFd<'_>], timeout_ms: i32) {
    use rustix::event::{PollFd, PollFlags, Timespec};
    let mut p: Vec<PollFd<'_>> = fds
        .iter()
        .map(|fd| PollFd::new(fd, PollFlags::IN))
        .collect();
    let t = Timespec {
        tv_sec: i64::from(timeout_ms / 1000),
        tv_nsec: i64::from(timeout_ms % 1000) * 1_000_000,
    };
    let _ = rustix::event::poll(&mut p, Some(&t));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names_round_trip() {
        for (n, s) in KEYS {
            assert_eq!(keysym_for(n), Some(*s));
            assert_eq!(name_for_keysym(*s), Some(*n));
        }
        assert_eq!(keysym_for("rightctrl"), Some(0xffe4));
        assert_eq!(keysym_for("Nope"), None);
    }
}
