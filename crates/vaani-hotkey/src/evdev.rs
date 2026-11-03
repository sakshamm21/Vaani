//! evdev backend: read key events straight from the keyboards in /dev/input. It works on
//! every Wayland compositor (and on X11), sees any key including a lone Right Ctrl, and
//! reports key-up. It needs read access to /dev/input, which on most distros means being in
//! the `input` group. Keyboards plugged in later are picked up within two seconds.

use std::collections::BTreeMap;
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use ::evdev::Device as RawDevice;
use ::evdev::{EventType, KeyCode};
use crossbeam_channel::Sender;

use crate::{
    HotkeyError, HotkeyEvent, Listener, Watch, keysym_for, name_for_keysym, wait_readable,
};

/// The evdev key code for a pickable key name.
fn code_for(name: &str) -> Option<KeyCode> {
    let sym = keysym_for(name)?;
    Some(match sym {
        0xffe4 => KeyCode::KEY_RIGHTCTRL,
        0xffea => KeyCode::KEY_RIGHTALT,
        0xffe2 => KeyCode::KEY_RIGHTSHIFT,
        0xffec => KeyCode::KEY_RIGHTMETA,
        0xffe3 => KeyCode::KEY_LEFTCTRL,
        0xffe9 => KeyCode::KEY_LEFTALT,
        0xffeb => KeyCode::KEY_LEFTMETA,
        0xff67 => KeyCode::KEY_COMPOSE,
        0xff63 => KeyCode::KEY_INSERT,
        0xff13 => KeyCode::KEY_PAUSE,
        0xff14 => KeyCode::KEY_SCROLLLOCK,
        // F1..F10 are contiguous in evdev, F11 and F12 are not.
        0xffbe..=0xffc7 => KeyCode::new(KeyCode::KEY_F1.0 + (sym - 0xffbe) as u16),
        0xffc8 => KeyCode::KEY_F11,
        0xffc9 => KeyCode::KEY_F12,
        _ => return None,
    })
}

fn name_for(code: KeyCode) -> Option<&'static str> {
    crate::KEYS
        .iter()
        .find(|(n, _)| code_for(n) == Some(code))
        .and_then(|(_, s)| name_for_keysym(*s))
}

/// Our own virtual keyboard (vaani-inject) must not trigger the hotkey.
const OWN_DEVICE: &str = "Vaani virtual keyboard";

/// Open every keyboard we can read, skipping ones already open.
fn scan(open: &mut BTreeMap<PathBuf, RawDevice>) -> usize {
    let mut denied = 0;
    let Ok(dir) = std::fs::read_dir("/dev/input") else {
        return 0;
    };
    for entry in dir.flatten() {
        let path = entry.path();
        let is_event = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("event"));
        if !is_event || open.contains_key(&path) {
            continue;
        }
        match RawDevice::open(&path) {
            Ok(dev) => {
                let keyboard = dev.supported_keys().is_some_and(|k| {
                    k.contains(KeyCode::KEY_A) || k.contains(KeyCode::KEY_RIGHTCTRL)
                });
                if keyboard && dev.name() != Some(OWN_DEVICE) && dev.set_nonblocking(true).is_ok() {
                    tracing::debug!(path = %path.display(), name = ?dev.name(), "reading keyboard");
                    open.insert(path, dev);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => denied += 1,
            Err(_) => {}
        }
    }
    denied
}

/// Whether evdev can be used: at least one keyboard is readable.
pub fn available() -> bool {
    let mut open = BTreeMap::new();
    scan(&mut open);
    !open.is_empty()
}

/// Key events (code, value) from all keyboards, waiting up to `timeout_ms`. Devices that
/// were unplugged are dropped.
fn read(open: &mut BTreeMap<PathBuf, RawDevice>, timeout_ms: i32) -> Vec<(KeyCode, i32)> {
    {
        let fds: Vec<_> = open.values().map(|d| d.as_fd()).collect();
        wait_readable(&fds, timeout_ms);
    }
    let mut out = Vec::new();
    let mut gone = Vec::new();
    for (path, dev) in open.iter_mut() {
        match dev.fetch_events() {
            Ok(events) => out.extend(
                events
                    .filter(|e| e.event_type() == EventType::KEY)
                    .map(|e| (KeyCode::new(e.code()), e.value())),
            ),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => gone.push(path.clone()),
        }
    }
    for p in gone {
        tracing::debug!(path = %p.display(), "keyboard went away");
        open.remove(&p);
    }
    out
}

pub fn listen(watch: Watch, tx: Sender<HotkeyEvent>) -> Result<Listener, HotkeyError> {
    code_for(&watch.key).ok_or_else(|| HotkeyError::UnknownKey(watch.key.clone()))?;
    let mut open = BTreeMap::new();
    let denied = scan(&mut open);
    if open.is_empty() {
        return Err(HotkeyError::NoInputAccess(denied));
    }
    let key = Arc::new(Mutex::new(watch));
    let stop = Arc::new(AtomicBool::new(false));
    let listener = Listener {
        key: key.clone(),
        stop: stop.clone(),
    };
    thread::Builder::new()
        .name("hotkey".into())
        .spawn(move || {
            let mut current = Watch::default();
            let mut hot = None;
            let mut cmd = None;
            // Several keyboards can hold the key at once; only the first press and the last
            // release count.
            let mut held = 0usize;
            let mut cmd_held = 0usize;
            let mut rescan = Instant::now();
            while !stop.load(Ordering::Relaxed) {
                if let Ok(k) = key.lock() {
                    if *k != current {
                        current = k.clone();
                        hot = code_for(&current.key);
                        cmd = code_for(&current.command);
                        held = 0;
                        cmd_held = 0;
                        tracing::info!(key = %current.key, command = %current.command, "hotkey set (evdev)");
                    }
                }
                if rescan.elapsed() > Duration::from_secs(2) {
                    scan(&mut open);
                    rescan = Instant::now();
                }
                for (code, value) in read(&mut open, 250) {
                    let send = |ev| {
                        let _ = tx.send(ev);
                    };
                    // value: 1 press, 0 release, 2 auto-repeat (ignored)
                    if Some(code) == hot {
                        if let Some(ev) = track(&mut held, value, HotkeyEvent::Down, HotkeyEvent::Up) {
                            send(ev);
                        }
                    } else if Some(code) == cmd {
                        if let Some(ev) =
                            track(&mut cmd_held, value, HotkeyEvent::CommandDown, HotkeyEvent::CommandUp)
                        {
                            send(ev);
                        }
                    } else if code == KeyCode::KEY_ESC && value == 1 {
                        send(HotkeyEvent::Escape);
                    }
                }
            }
        })
        .map_err(|e| HotkeyError::Io(e.to_string()))?;
    Ok(listener)
}

/// Count presses of one key across keyboards; the first press and the last release count.
fn track(held: &mut usize, value: i32, down: HotkeyEvent, up: HotkeyEvent) -> Option<HotkeyEvent> {
    match value {
        1 => {
            *held += 1;
            (*held == 1).then_some(down)
        }
        0 if *held > 0 => {
            *held -= 1;
            (*held == 0).then_some(up)
        }
        _ => None,
    }
}

pub fn capture_next_key(timeout: Duration) -> Result<Option<&'static str>, HotkeyError> {
    let mut open = BTreeMap::new();
    let denied = scan(&mut open);
    if open.is_empty() {
        return Err(HotkeyError::NoInputAccess(denied));
    }
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        for (code, value) in read(&mut open, 50) {
            if value == 1 {
                if let Some(name) = name_for(code) {
                    return Ok(Some(name));
                }
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_has_an_evdev_code() {
        for (n, _) in crate::KEYS {
            let code = code_for(n).unwrap_or_else(|| panic!("{n}"));
            assert_eq!(name_for(code), Some(*n));
        }
        assert_eq!(code_for("F10"), Some(KeyCode::KEY_F10));
        assert_eq!(code_for("F11"), Some(KeyCode::KEY_F11));
    }
}
