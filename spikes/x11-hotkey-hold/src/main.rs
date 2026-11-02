//! Spike: detect press-and-hold of a hotkey on X11 with XInput2 raw events.
//!
//! Question: can we see a key go down and come back up, globally, without grabbing it
//! (so the key still works in other apps) and without being fooled by key auto-repeat?
//!
//! Run:   cargo run -- [KEYSYM_NAME] [--seconds N]
//!        KEYSYM_NAME defaults to Control_R. Also try: Alt_R, Super_L, F9.
//! Then hold the key for a second or two and release it, a few times. Press Esc while
//! holding to check cancel detection. Ctrl+C to quit.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xinput::{self, ConnectionExt as _, KeyEventFlags};
use x11rb::protocol::xproto::ConnectionExt as _;

fn keysym_for(name: &str) -> Option<u32> {
    Some(match name {
        "Control_R" => 0xffe4,
        "Control_L" => 0xffe3,
        "Alt_R" => 0xffea,
        "Alt_L" => 0xffe9,
        "Super_L" => 0xffeb,
        "Super_R" => 0xffec,
        "Shift_R" => 0xffe2,
        "Escape" => 0xff1b,
        "space" => 0x20,
        f if f.starts_with('F') => 0xffbe + f[1..].parse::<u32>().ok()?.checked_sub(1)?,
        _ => return None,
    })
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let key_name = args
        .iter()
        .find(|a| !a.starts_with("--") && a.parse::<u64>().is_err())
        .cloned()
        .unwrap_or_else(|| "Control_R".into());
    let seconds: Option<u64> = args
        .iter()
        .position(|a| a == "--seconds")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok());

    let (conn, screen_num) = x11rb::connect(None).context("cannot connect to the X server")?;
    let root = conn.setup().roots[screen_num].root;

    let v = conn
        .xinput_xi_query_version(2, 2)?
        .reply()
        .context("XInput2 not available")?;
    println!("XInput version {}.{}", v.major_version, v.minor_version);
    if (v.major_version, v.minor_version) < (2, 0) {
        bail!("need XInput 2.0 or newer");
    }

    // keysym -> keycodes
    let setup = conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let map = conn.get_keyboard_mapping(min, max - min + 1)?.reply()?;
    let per = map.keysyms_per_keycode as usize;
    let codes_for = |sym: u32| -> Vec<u32> {
        map.keysyms
            .chunks(per)
            .enumerate()
            .filter(|(_, syms)| syms.contains(&sym))
            .map(|(i, _)| u32::from(min) + i as u32)
            .collect()
    };
    let sym = keysym_for(&key_name).with_context(|| format!("unknown key name {key_name}"))?;
    let hot = codes_for(sym);
    let esc = codes_for(0xff1b);
    if hot.is_empty() {
        bail!("no keycode for {key_name} on this keyboard layout");
    }
    println!("Watching {key_name} (keycodes {hot:?}); Esc = {esc:?}. Hold and release it.");

    // Raw events are delivered from all devices to the root window, without a grab.
    conn.xinput_xi_select_events(
        root,
        &[xinput::EventMask {
            deviceid: 1, // XIAllMasterDevices
            mask: vec![xinput::XIEventMask::RAW_KEY_PRESS | xinput::XIEventMask::RAW_KEY_RELEASE],
        }],
    )?
    .check()?;
    conn.flush()?;

    let start = Instant::now();
    let mut down_at: Option<Instant> = None;
    let mut repeats = 0u32;
    let mut holds = 0u32;
    loop {
        if let Some(s) = seconds {
            if start.elapsed() > Duration::from_secs(s) {
                break;
            }
        }
        let Some(event) = conn.poll_for_event()? else {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        };
        let t = start.elapsed().as_millis();
        match event {
            Event::XinputRawKeyPress(e) if hot.contains(&e.detail) => {
                if e.flags.contains(KeyEventFlags::KEY_REPEAT) || down_at.is_some() {
                    repeats += 1;
                    continue;
                }
                down_at = Some(Instant::now());
                println!(
                    "[{t:>6} ms] DOWN  (device {}, source {})",
                    e.deviceid, e.sourceid
                );
            }
            Event::XinputRawKeyRelease(e) if hot.contains(&e.detail) => {
                if let Some(d) = down_at.take() {
                    holds += 1;
                    println!(
                        "[{t:>6} ms] UP    held {} ms (ignored {repeats} auto-repeats)",
                        d.elapsed().as_millis()
                    );
                    repeats = 0;
                }
            }
            Event::XinputRawKeyPress(e) if esc.contains(&e.detail) && down_at.is_some() => {
                println!("[{t:>6} ms] ESC while holding -> would cancel");
            }
            _ => {}
        }
    }
    println!("RESULT holds={holds}");
    Ok(())
}
