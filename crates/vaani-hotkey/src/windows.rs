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
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY,
};
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

const CTRL: [u32; 2] = [0xa2, 0xa3];
const ALT: [u32; 2] = [0xa4, 0xa5];
const SHIFT: [u32; 2] = [0xa0, 0xa1];
const WIN: [u32; 2] = [0x5b, 0x5c];

/// The keys a chord modifier name stands for: either side counts.
fn modifier_vks(name: &str) -> Option<&'static [u32]> {
    Some(match name.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => &CTRL,
        "alt" => &ALT,
        "shift" => &SHIFT,
        "super" | "win" => &WIN,
        _ => return None,
    })
}

/// The generic chord name of a modifier key ("Ctrl" for either Ctrl), if it is one.
fn modifier_name(vk: u32) -> Option<&'static str> {
    [
        ("Ctrl", &CTRL),
        ("Alt", &ALT),
        ("Shift", &SHIFT),
        ("Super", &WIN),
    ]
    .into_iter()
    .find(|(_, vks)| vks.contains(&vk))
    .map(|(n, _)| n)
}

/// A key or chord as groups of virtual keys: the chord is held when every group has a key
/// down. "RightCtrl" is one group of one key; "Ctrl+Super" is {LCtrl, RCtrl} + {LWin, RWin}.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Chord(Vec<Vec<u32>>);

impl Chord {
    fn held(&self, held: &[u32]) -> bool {
        !self.0.is_empty() && self.0.iter().all(|g| g.iter().any(|v| held.contains(v)))
    }

    fn contains(&self, vk: u32) -> bool {
        self.0.iter().any(|g| g.contains(&vk))
    }

    /// Releasing Alt or Win on its own opens a menu (the app's menu bar, the Start menu).
    fn opens_menu(&self) -> bool {
        self.0
            .iter()
            .flatten()
            .any(|v| ALT.contains(v) || WIN.contains(v))
    }
}

/// Parse "RightCtrl", "F9" or a chord such as "Ctrl+Super" or "Ctrl+Alt+F9": one or more
/// modifiers (either side) plus at most one other pickable key.
pub(crate) fn parse(name: &str) -> Option<Chord> {
    let parts: Vec<&str> = name.split('+').map(str::trim).collect();
    if let [one] = parts[..] {
        return vk_for(one).map(|vk| Chord(vec![vec![vk]]));
    }
    let mut groups: Vec<Vec<u32>> = Vec::new();
    let mut others = 0;
    for p in parts {
        let g = match modifier_vks(p) {
            Some(vks) => vks.to_vec(),
            None => {
                others += 1;
                vec![vk_for(p)?]
            }
        };
        if groups.contains(&g) {
            return None;
        }
        groups.push(g);
    }
    (others <= 1 && groups.len() >= 2).then_some(Chord(groups))
}

/// The name to store for the keys pressed together during a capture, in press order.
fn name_for_keys(keys: &[u32]) -> Option<String> {
    if let [vk] = keys {
        return name_for_vk(*vk).map(str::to_string);
    }
    let mut mods: Vec<&str> = Vec::new();
    let mut other = None;
    for &vk in keys {
        match modifier_name(vk) {
            Some(m) if !mods.contains(&m) => mods.push(m),
            Some(_) => {}
            None if other.is_none() => other = Some(name_for_vk(vk)?),
            None => return None,
        }
    }
    if mods.is_empty() || mods.len() + usize::from(other.is_some()) < 2 {
        return None;
    }
    // Ctrl, Alt, Shift, Super, then the key, the order Windows shows shortcuts in.
    let order = ["Ctrl", "Alt", "Shift", "Super"];
    mods.sort_by_key(|m| order.iter().position(|o| o == m));
    let mut parts: Vec<&str> = mods;
    parts.extend(other);
    Some(parts.join("+"))
}

enum Sink {
    Listen {
        watch: Arc<Mutex<Watch>>,
        stop: Arc<AtomicBool>,
        tx: Sender<HotkeyEvent>,
        down: bool,
        cmd_down: bool,
    },
    Capture {
        tx: Sender<String>,
        until: Instant,
        /// Pickable keys pressed so far, in order.
        keys: Vec<u32>,
        /// A key that can't be part of a shortcut (a letter) was pressed with them.
        spoiled: bool,
    },
}

static SINKS: Mutex<Vec<Sink>> = Mutex::new(Vec::new());
/// Keys down right now, as the hook has seen them.
static HELD: Mutex<Vec<u32>> = Mutex::new(Vec::new());
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

/// The scan code Windows gives the Left Ctrl press it sends ahead of AltGr (Right Alt on
/// many European layouts). That press isn't a real Ctrl, so it is ignored.
const ALTGR_FAKE_CTRL_SCAN: u32 = 0x21d;

unsafe extern "system" fn proc_(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: for WH_KEYBOARD_LL with code >= 0, lparam points to a KBDLLHOOKSTRUCT.
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        // Ignore the keys Vaani itself sends. Keys other programs inject (remappers,
        // accessibility tools, automated tests) still count.
        let fake_ctrl = info.vkCode == CTRL[0] && info.scanCode == ALTGR_FAKE_CTRL_SCAN;
        if info.dwExtraInfo != VAANI_KEY_TAG && !fake_ctrl {
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

/// Update [`HELD`] for one key event and return a copy.
fn track(vk: u32, down: bool) -> Vec<u32> {
    let Ok(mut held) = HELD.lock() else {
        return vec![vk];
    };
    if down {
        if !held.contains(&vk) {
            held.push(vk);
        }
    } else {
        held.retain(|&k| k != vk);
    }
    // A key-up can go missing (Win+L locks the screen with the keys down, a UAC prompt
    // takes the keyboard), which would leave a chord stuck "held". Drop any key Windows
    // says is up. The key of this event isn't in that state yet, so it is kept as is.
    held.retain(|&k| {
        // SAFETY: GetAsyncKeyState only reads the key state; any vk value is valid.
        k == vk || unsafe { GetAsyncKeyState(k as i32) } < 0
    });
    held.clone()
}

fn on_key(vk: u32, down: bool) {
    let held = track(vk, down);
    let Ok(mut sinks) = SINKS.lock() else { return };
    let now = Instant::now();
    sinks.retain(|s| !matches!(s, Sink::Capture { until, .. } if *until < now));
    // While the settings window waits for a new shortcut, the keys pressed are for it, not
    // for dictation.
    let capturing = sinks.iter().any(|s| matches!(s, Sink::Capture { .. }));
    sinks.retain_mut(|sink| match sink {
        Sink::Capture {
            tx, keys, spoiled, ..
        } => {
            if down {
                if parse_one(vk) {
                    if !keys.contains(&vk) {
                        keys.push(vk);
                    }
                } else if vk != VK_ESCAPE {
                    *spoiled = true;
                }
                return true;
            }
            // The first key let go ends the shortcut.
            if keys.is_empty() && !*spoiled {
                return true;
            }
            let name = (!*spoiled).then(|| name_for_keys(keys)).flatten();
            keys.clear();
            *spoiled = false;
            match name {
                Some(n) => {
                    let _ = tx.send(n);
                    false
                }
                // Not usable (a lone letter, Ctrl+C): keep waiting for another try.
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
            if capturing {
                return true;
            }
            let (key, command) = match watch.lock() {
                Ok(w) => (parse(&w.key), parse(&w.command)),
                Err(_) => return true,
            };
            let mut ok = true;
            let mut send = |ev| ok &= tx.send(ev).is_ok();
            if let Some(k) = &key {
                let active = k.held(&held);
                if active != *key_down {
                    *key_down = active;
                    if active {
                        if k.opens_menu() {
                            mask_menu();
                        }
                        send(HotkeyEvent::Down);
                    } else {
                        send(HotkeyEvent::Up);
                    }
                } else if active && down && !k.contains(vk) && vk != VK_ESCAPE {
                    // Another key joined the hotkey (Ctrl+Win+D, Right Ctrl+C): that is a
                    // different shortcut, so drop the recording.
                    send(if k.0.len() > 1 {
                        HotkeyEvent::Escape
                    } else {
                        HotkeyEvent::Chord
                    });
                }
            }
            if let Some(c) = &command {
                let active = c.held(&held);
                if active != *cmd_down {
                    *cmd_down = active;
                    if active {
                        if c.opens_menu() {
                            mask_menu();
                        }
                        send(HotkeyEvent::CommandDown);
                    } else {
                        send(HotkeyEvent::CommandUp);
                    }
                }
            }
            if vk == VK_ESCAPE && down {
                send(HotkeyEvent::Escape);
            }
            ok
        }
    });
}

/// Whether a key can be part of a shortcut.
fn parse_one(vk: u32) -> bool {
    modifier_name(vk).is_some() || name_for_vk(vk).is_some()
}

/// Send one press of an unassigned key (0xE8) while Alt or Win is held, so letting go of
/// it doesn't open the Start menu or an app's menu bar. Windows opens those only when no
/// other key was pressed in between. Sent from a short-lived thread: the hook procedure
/// must not inject input itself.
fn mask_menu() {
    let _ = std::thread::Builder::new()
        .name("hotkey-mask".into())
        .spawn(|| {
            let key = |flags| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(0xe8),
                        wScan: 0,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: VAANI_KEY_TAG,
                    },
                },
            };
            let inputs = [key(KEYBD_EVENT_FLAGS(0)), key(KEYEVENTF_KEYUP)];
            // SAFETY: a plain SendInput of two keyboard events from a local array.
            unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
        });
}

pub(crate) fn listen(watch: Watch, tx: Sender<HotkeyEvent>) -> Result<Listener, HotkeyError> {
    parse(&watch.key).ok_or_else(|| HotkeyError::UnknownKey(watch.key.clone()))?;
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

/// Wait for the next shortcut: a single key, or keys pressed together, reported when the
/// first of them is let go.
pub(crate) fn capture_next_key(timeout: Duration) -> Result<Option<String>, HotkeyError> {
    ensure_hook()?;
    let (tx, rx) = crossbeam_channel::bounded(1);
    if let Ok(mut sinks) = SINKS.lock() {
        sinks.push(Sink::Capture {
            tx,
            until: Instant::now() + timeout,
            keys: Vec::new(),
            spoiled: false,
        });
    }
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

    #[test]
    fn chords_parse_and_match() {
        let c = parse("Ctrl+Super").unwrap();
        assert!(c.held(&[0xa2, 0x5b]));
        assert!(c.held(&[0x5c, 0xa3]));
        assert!(!c.held(&[0xa2]));
        assert!(c.opens_menu());
        assert!(parse("ctrl + alt + F9").is_some());
        assert!(parse("RightCtrl").unwrap().held(&[0xa3]));
        assert!(!parse("RightCtrl").unwrap().held(&[0xa2]));
        assert!(!parse("RightCtrl").unwrap().opens_menu());
        assert_eq!(parse("Ctrl+Ctrl"), None);
        assert_eq!(parse("F9+F10"), None);
        assert_eq!(parse("Ctrl+Q"), None);
        assert_eq!(parse("Ctrl"), None);
    }

    #[test]
    fn captured_keys_get_names() {
        assert_eq!(name_for_keys(&[0xa3]).as_deref(), Some("RightCtrl"));
        assert_eq!(name_for_keys(&[0x5b, 0xa2]).as_deref(), Some("Ctrl+Super"));
        assert_eq!(name_for_keys(&[0xa4, 0x78]).as_deref(), Some("Alt+F9"));
        assert_eq!(name_for_keys(&[0xa0]), None);
        assert_eq!(name_for_keys(&[0x78, 0x79]), None);
        for n in ["Ctrl+Super", "Alt+F9", "Ctrl+Alt+Shift"] {
            assert!(parse(n).is_some(), "{n}");
        }
    }
}
