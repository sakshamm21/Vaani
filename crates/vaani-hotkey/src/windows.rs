//! Windows backend: a low-level keyboard hook (`WH_KEYBOARD_LL`).
//!
//! The hook sees every key press system-wide before the focused app does, without taking
//! the key away from it, and reports key-up. One hook thread is installed the first time it
//! is needed and lives for the rest of the process; listeners and the settings window's
//! "press a key" capture register themselves as sinks it forwards to.
//!
//! The hook procedure must return quickly (Windows drops hooks that take longer than about
//! a second), so it only looks up the key and sends on a channel.

#![allow(unsafe_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crossbeam_channel::Sender;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HHOOK, KBDLLHOOKSTRUCT, MSG, SetWindowsHookExW,
    TranslateMessage, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::{HotkeyError, HotkeyEvent, Listener, Watch, keysym_for, name_for_keysym};

const VK_ESCAPE: u32 = 0x1b;
/// The tag vaani-inject puts on the keys it sends (Ctrl+V, Backspace, typed text).
const VAANI_KEY_TAG: usize = 0x5641_414E;

/// The Windows virtual-key code for a pickable key name.
pub(crate) fn vk_for(name: &str) -> Option<u32> {
    Some(match keysym_for(name)? {
        0xffe4 => 0xa3,                             // VK_RCONTROL
        0xffea => 0xa5,                             // VK_RMENU
        0xffe2 => 0xa1,                             // VK_RSHIFT
        0xffec => 0x5c,                             // VK_RWIN
        0xffe3 => 0xa2,                             // VK_LCONTROL
        0xffe9 => 0xa4,                             // VK_LMENU
        0xffeb => 0x5b,                             // VK_LWIN
        0xff67 => 0x5d,                             // VK_APPS
        0xff63 => 0x2d,                             // VK_INSERT
        0xff13 => 0x13,                             // VK_PAUSE
        0xff14 => 0x91,                             // VK_SCROLL
        f @ 0xffbe..=0xffc9 => 0x70 + (f - 0xffbe), // VK_F1..VK_F12
        _ => return None,
    })
}

fn name_for_vk(vk: u32) -> Option<&'static str> {
    crate::KEYS
        .iter()
        .find(|(n, _)| vk_for(n) == Some(vk))
        .and_then(|(_, s)| name_for_keysym(*s))
}

enum Sink {
    Listen {
        watch: Arc<Mutex<Watch>>,
        stop: Arc<AtomicBool>,
        tx: Sender<HotkeyEvent>,
        down: bool,
        cmd_down: bool,
    },
    Capture(Sender<&'static str>),
}

static SINKS: Mutex<Vec<Sink>> = Mutex::new(Vec::new());
static HOOK: OnceLock<Result<(), String>> = OnceLock::new();

/// Install the hook thread once. Later calls return the first result.
fn ensure_hook() -> Result<(), HotkeyError> {
    HOOK.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("hotkey".into())
            .spawn(move || {
                // SAFETY: a plain hook install on this thread; `proc_` has the signature
                // Windows expects and the thread runs a message loop below, which the
                // hook needs to be called.
                let hook: windows::core::Result<HHOOK> =
                    unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(proc_), None, 0) };
                match hook {
                    Ok(_) => {
                        let _ = tx.send(Ok(()));
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e.to_string()));
                        return;
                    }
                }
                let mut msg = MSG::default();
                // SAFETY: standard message loop on the thread that owns the hook.
                unsafe {
                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            });
        match spawned {
            Ok(_) => rx
                .recv_timeout(Duration::from_secs(5))
                .unwrap_or_else(|_| Err("the hook thread did not start".into())),
            Err(e) => Err(e.to_string()),
        }
    })
    .clone()
    .map_err(HotkeyError::Hook)
}

unsafe extern "system" fn proc_(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: for WH_KEYBOARD_LL with code >= 0, lparam points to a KBDLLHOOKSTRUCT.
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        // Ignore the keys Vaani itself sends. Keys other programs inject (remappers,
        // accessibility tools, automated tests) still count.
        if info.dwExtraInfo != VAANI_KEY_TAG {
            let msg = wparam.0 as u32;
            let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
            if down || up {
                on_key(info.vkCode, down);
            }
        }
    }
    // SAFETY: always pass the key on, so the app with focus still gets it.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn on_key(vk: u32, down: bool) {
    let Ok(mut sinks) = SINKS.lock() else { return };
    sinks.retain_mut(|sink| match sink {
        Sink::Capture(tx) => {
            if !down {
                return true;
            }
            match name_for_vk(vk) {
                Some(name) => {
                    let _ = tx.send(name);
                    false
                }
                None => true,
            }
        }
        Sink::Listen {
            watch,
            stop,
            tx,
            down: key_down,
            cmd_down,
        } => {
            if stop.load(Ordering::Relaxed) {
                return false;
            }
            let (key, command) = match watch.lock() {
                Ok(w) => (vk_for(&w.key), vk_for(&w.command)),
                Err(_) => return true,
            };
            let send = |ev| tx.send(ev).is_ok();
            if Some(vk) == key {
                // Held keys repeat WM_KEYDOWN: only the first press counts.
                if down && !*key_down {
                    *key_down = true;
                    send(HotkeyEvent::Down)
                } else if !down && *key_down {
                    *key_down = false;
                    send(HotkeyEvent::Up)
                } else {
                    true
                }
            } else if Some(vk) == command {
                if down && !*cmd_down {
                    *cmd_down = true;
                    send(HotkeyEvent::CommandDown)
                } else if !down && *cmd_down {
                    *cmd_down = false;
                    send(HotkeyEvent::CommandUp)
                } else {
                    true
                }
            } else if vk == VK_ESCAPE && down {
                send(HotkeyEvent::Escape)
            } else {
                true
            }
        }
    });
}

pub(crate) fn listen(watch: Watch, tx: Sender<HotkeyEvent>) -> Result<Listener, HotkeyError> {
    vk_for(&watch.key).ok_or_else(|| HotkeyError::UnknownKey(watch.key.clone()))?;
    ensure_hook()?;
    tracing::info!(key = %watch.key, command = %watch.command, "hotkey set");
    let key = Arc::new(Mutex::new(watch));
    let stop = Arc::new(AtomicBool::new(false));
    if let Ok(mut sinks) = SINKS.lock() {
        sinks.push(Sink::Listen {
            watch: key.clone(),
            stop: stop.clone(),
            tx,
            down: false,
            cmd_down: false,
        });
    }
    Ok(Listener { key, stop })
}

pub(crate) fn capture_next_key(timeout: Duration) -> Result<Option<&'static str>, HotkeyError> {
    ensure_hook()?;
    let (tx, rx) = crossbeam_channel::bounded(1);
    if let Ok(mut sinks) = SINKS.lock() {
        sinks.push(Sink::Capture(tx));
    }
    // A sink that never fired is dropped on the next key press, when its send fails.
    Ok(rx.recv_timeout(timeout).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_has_a_virtual_key() {
        for (n, _) in crate::KEYS {
            let vk = vk_for(n).unwrap_or_else(|| panic!("{n}"));
            assert_eq!(name_for_vk(vk), Some(*n));
        }
        assert_eq!(vk_for("F12"), Some(0x7b));
    }
}
