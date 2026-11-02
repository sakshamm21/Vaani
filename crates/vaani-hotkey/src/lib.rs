//! Global hotkey detection.
//!
//! X11 backend: XInput2 raw key events on the root window. They arrive from every keyboard
//! without grabbing the key, so the key keeps working in other apps, and they report key-up,
//! which push-to-talk needs. Auto-repeat presses are ignored. Esc while dictating cancels.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crossbeam_channel::Sender;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xinput::{self, ConnectionExt as _, KeyEventFlags};
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

#[derive(Debug, thiserror::Error)]
pub enum HotkeyError {
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
    key: Arc<Mutex<String>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
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
            *k = name.to_string();
        }
        Ok(())
    }
}

/// Start listening for `key` on X11. Events go to `tx`.
pub fn listen(key: &str, tx: Sender<HotkeyEvent>) -> Result<Listener, HotkeyError> {
    keysym_for(key).ok_or_else(|| HotkeyError::UnknownKey(key.into()))?;
    let (conn, _root) = connect()?;
    let keymap = Keymap::load(&conn)?;
    if keymap.codes(keysym_for(key).unwrap_or(0)).is_empty() {
        return Err(HotkeyError::NotOnKeyboard(key.into()));
    }
    let key = Arc::new(Mutex::new(key.to_string()));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let listener = Listener {
        key: key.clone(),
        stop: stop.clone(),
    };
    thread::Builder::new()
        .name("hotkey".into())
        .spawn(move || {
            let esc = keymap.codes(ESCAPE);
            let mut current = String::new();
            let mut hot: Vec<u32> = Vec::new();
            let mut down = false;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                if let Ok(k) = key.lock() {
                    if *k != current {
                        current = k.clone();
                        hot = keymap.codes(keysym_for(&current).unwrap_or(0));
                        down = false;
                        tracing::info!(key = %current, ?hot, "hotkey set");
                    }
                }
                let event = match conn.poll_for_event() {
                    Ok(Some(e)) => e,
                    Ok(None) => {
                        thread::sleep(Duration::from_millis(4));
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

/// Wait for the next key press from a pickable key and return its name (for the settings
/// "press a key" button). Returns None after `timeout`.
pub fn capture_next_key(timeout: Duration) -> Result<Option<&'static str>, HotkeyError> {
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
            None => thread::sleep(Duration::from_millis(5)),
        }
    }
    Ok(None)
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
