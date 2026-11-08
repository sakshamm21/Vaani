//! Global hotkey detection.
//!
//! Linux: XInput2 raw key events on X11 (see `x11`); on Wayland the keyboards are read
//! through evdev when /dev/input is readable, and otherwise the GlobalShortcuts portal is
//! used (see those modules).
//!
//! Windows: a low-level keyboard hook (see `windows`).
//!
//! Every backend sees the key without grabbing it, so it keeps working in other apps, and
//! reports key-up, which push-to-talk needs. Auto-repeat presses are ignored. Esc while
//! dictating cancels.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::Sender;

#[cfg(target_os = "linux")]
mod evdev;
#[cfg(target_os = "linux")]
mod hyprland;
#[cfg(target_os = "linux")]
mod portal;
#[cfg(windows)]
mod windows;
#[cfg(target_os = "linux")]
mod x11;

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
    #[error("keyboard hook: {0}")]
    Hook(String),
}

#[cfg(target_os = "linux")]
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

/// Whether `name` is a shortcut this platform can listen for: one of [`KEYS`], or on
/// Windows also a chord of modifiers such as "Ctrl+Super" (Ctrl + Win), "Ctrl+Alt" or
/// "Alt+F9".
pub fn is_valid(name: &str) -> bool {
    #[cfg(windows)]
    return windows::parse(name).is_some();
    #[cfg(not(windows))]
    keysym_for(name).is_some()
}

pub fn keysym_for(name: &str) -> Option<u32> {
    KEYS.iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, s)| *s)
}

pub fn name_for_keysym(sym: u32) -> Option<&'static str> {
    KEYS.iter().find(|(_, s)| *s == sym).map(|(n, _)| *n)
}

/// A running hotkey listener. Change the key with [`Listener::set_key`]; drop to stop.
pub struct Listener {
    pub(crate) key: Arc<Mutex<Watch>>,
    pub(crate) stop: Arc<std::sync::atomic::AtomicBool>,
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
    /// A low-level keyboard hook (Windows).
    Hook,
}

impl Backend {
    pub fn label(self) -> &'static str {
        match self {
            Self::X11 => "X11 (XInput2)",
            Self::Evdev => "evdev (/dev/input)",
            Self::Portal => "GlobalShortcuts portal",
            Self::Hook => "keyboard hook",
        }
    }
}

#[cfg(target_os = "linux")]
fn wayland_session() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
        || std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v == "wayland")
}

/// The backend [`listen`] would use right now.
#[cfg(windows)]
pub fn backend() -> Backend {
    Backend::Hook
}

/// The backend [`listen`] would use right now.
#[cfg(target_os = "linux")]
pub fn backend() -> Backend {
    if !wayland_session() {
        Backend::X11
    } else if evdev::available() {
        Backend::Evdev
    } else {
        Backend::Portal
    }
}

/// The dictation shortcut as the desktop shows it, when the portal backend is in use.
pub fn portal_trigger() -> Option<String> {
    #[cfg(target_os = "linux")]
    return portal::trigger();
    #[cfg(not(target_os = "linux"))]
    None
}

/// Whether the GlobalShortcuts portal answers (for `vaani doctor`).
pub fn portal_available() -> bool {
    #[cfg(target_os = "linux")]
    return portal::available();
    #[cfg(not(target_os = "linux"))]
    false
}

/// Start listening for `key`, with `backend` or else the best one for this session.
/// Events go to `tx`.
pub fn listen(
    key: &str,
    command_key: &str,
    backend: Option<Backend>,
    tx: Sender<HotkeyEvent>,
) -> Result<(Listener, Backend), HotkeyError> {
    if !command_key.is_empty() && !is_valid(command_key) {
        return Err(HotkeyError::UnknownKey(command_key.into()));
    }
    let watch = Watch {
        key: key.to_string(),
        command: command_key.to_string(),
    };
    #[cfg(target_os = "linux")]
    {
        let b = backend.unwrap_or_else(self::backend);
        let l = match b {
            Backend::X11 => x11::listen(watch, tx)?,
            Backend::Evdev => evdev::listen(watch, tx)?,
            Backend::Portal => portal::listen(watch, tx)?,
            Backend::Hook => {
                return Err(HotkeyError::Hook("only available on Windows".into()));
            }
        };
        Ok((l, b))
    }
    #[cfg(windows)]
    {
        let _ = backend;
        Ok((windows::listen(watch, tx)?, Backend::Hook))
    }
}

/// Wait for the next key press from a pickable key and return its name (for the settings
/// "press a key" button). On Windows keys pressed together give a chord ("Ctrl+Super").
/// Returns None after `timeout`.
pub fn capture_next_key(timeout: Duration) -> Result<Option<String>, HotkeyError> {
    match backend() {
        #[cfg(target_os = "linux")]
        Backend::X11 => Ok(x11::capture_next_key(timeout)?.map(str::to_string)),
        #[cfg(target_os = "linux")]
        Backend::Evdev => Ok(evdev::capture_next_key(timeout)?.map(str::to_string)),
        Backend::Portal => Err(HotkeyError::Portal(
            "change the shortcut in your desktop's keyboard settings".into(),
        )),
        #[cfg(windows)]
        Backend::Hook => windows::capture_next_key(timeout),
        #[allow(unreachable_patterns)]
        _ => Ok(None),
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Listener {
    pub fn set_key(&self, name: &str) -> Result<(), HotkeyError> {
        if !is_valid(name) {
            return Err(HotkeyError::UnknownKey(name.into()));
        }
        if let Ok(mut k) = self.key.lock() {
            k.key = name.to_string();
        }
        Ok(())
    }

    /// Set the command-mode key; empty turns it off.
    pub fn set_command_key(&self, name: &str) -> Result<(), HotkeyError> {
        if !name.is_empty() && !is_valid(name) {
            return Err(HotkeyError::UnknownKey(name.into()));
        }
        if let Ok(mut k) = self.key.lock() {
            k.command = name.to_string();
        }
        Ok(())
    }
}

/// Block until any of `fds` is readable or `timeout_ms` passes (no busy polling).
#[cfg(target_os = "linux")]
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
