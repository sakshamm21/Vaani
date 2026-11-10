//! X11 backend: XInput2 raw key events on the root window. They arrive from every keyboard
//! without grabbing the key.

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

use crate::{
    HotkeyError, HotkeyEvent, Listener, Watch, keysym_for, name_for_keysym, wait_readable, x,
};

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

/// Start listening for `key` on X11. Events go to `tx`.
pub(crate) fn listen(watch: Watch, tx: Sender<HotkeyEvent>) -> Result<Listener, HotkeyError> {
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
            let mut chorded = false;
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
                            chorded = false;
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
                    Event::XinputRawKeyPress(e)
                        if down && !chorded && !e.flags.contains(KeyEventFlags::KEY_REPEAT) =>
                    {
                        chorded = true;
                        send(HotkeyEvent::Chord);
                    }
                    _ => {}
                }
            }
        })
        .map_err(x)?;
    Ok(listener)
}

pub(crate) fn capture_next_key(timeout: Duration) -> Result<Option<&'static str>, HotkeyError> {
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
