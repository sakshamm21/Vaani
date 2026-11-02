//! Spike: insert text on X11 by owning the CLIPBOARD, pressing Ctrl+V with XTest, then
//! restoring whatever the user had on the clipboard before.
//!
//! Questions: does a synthetic Ctrl+V reach the focused app on Cinnamon? Can we tell when the
//! app has actually read our text (so the restore never races the paste)? Do every MIME type
//! and image on the old clipboard survive the round trip?
//!
//! Run:   cargo run -- ["text to insert"] [--terminal] [--hold SECONDS]
//!        You get 3 seconds to click into a text field (Firefox, Writer, VS Code, a terminal...).
//!        --terminal sends Ctrl+Shift+V instead of Ctrl+V.
//!        After the paste, the old clipboard is put back and kept available for --hold seconds
//!        (default 15): paste again by hand to check it's your old content.
//!
//! Note: X11 clipboards live in the owning process. The real app runs all the time, so it can
//! keep serving the restored clipboard; this spike stops serving when it exits.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use x11rb::CURRENT_TIME;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, SelectionNotifyEvent,
    SelectionRequestEvent, WindowClass,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

struct Atoms {
    clipboard: Atom,
    targets: Atom,
    utf8: Atom,
    text: Atom,
    text_plain: Atom,
    text_plain_utf8: Atom,
    incr: Atom,
    prop: Atom,
    timestamp: Atom,
    multiple: Atom,
    save_targets: Atom,
}

fn atom(conn: &RustConnection, name: &str) -> Result<Atom> {
    Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
}

fn atom_name(conn: &RustConnection, a: Atom) -> String {
    conn.get_atom_name(a)
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| String::from_utf8_lossy(&r.name).into_owned())
        .unwrap_or_else(|| format!("#{a}"))
}

/// What we offer for one selection ownership: target atom -> (property type, data).
type Offer = BTreeMap<Atom, (Atom, Vec<u8>)>;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let terminal = args.iter().any(|a| a == "--terminal");
    let hold: u64 = args
        .iter()
        .position(|a| a == "--hold")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(15);
    let text = args
        .iter()
        .enumerate()
        .find(|(i, a)| !a.starts_with("--") && (*i == 0 || args[i - 1] != "--hold"))
        .map(|(_, a)| a.clone())
        .unwrap_or_else(|| "Hello from Vaani — नमस्ते, ünïcödé ✓".into());

    let (conn, screen_num) = x11rb::connect(None).context("cannot connect to the X server")?;
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;
    let a = Atoms {
        clipboard: atom(&conn, "CLIPBOARD")?,
        targets: atom(&conn, "TARGETS")?,
        utf8: atom(&conn, "UTF8_STRING")?,
        text: atom(&conn, "TEXT")?,
        text_plain: atom(&conn, "text/plain")?,
        text_plain_utf8: atom(&conn, "text/plain;charset=utf-8")?,
        incr: atom(&conn, "INCR")?,
        prop: atom(&conn, "VAANI_SELECTION")?,
        timestamp: atom(&conn, "TIMESTAMP")?,
        multiple: atom(&conn, "MULTIPLE")?,
        save_targets: atom(&conn, "SAVE_TARGETS")?,
    };

    let win = conn.generate_id()?;
    conn.create_window(
        0,
        win,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        0,
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )?;
    conn.flush()?;

    // 1. Save the current clipboard, every target.
    let old_owner = conn.get_selection_owner(a.clipboard)?.reply()?.owner;
    let saved = if old_owner == x11rb::NONE {
        println!("Clipboard is empty (no owner).");
        Offer::new()
    } else {
        save_clipboard(&conn, win, &a)?
    };
    println!("Saved {} clipboard target(s):", saved.len());
    for (t, (ty, data)) in &saved {
        println!(
            "  {:<32} {:>8} bytes ({})",
            atom_name(&conn, *t),
            data.len(),
            atom_name(&conn, *ty)
        );
    }

    // 2. Offer our text.
    let mut ours = Offer::new();
    for t in [
        a.utf8,
        a.text_plain_utf8,
        AtomEnum::STRING.into(),
        a.text,
        a.text_plain,
    ] {
        ours.insert(
            t,
            (
                if t == a.text_plain_utf8 { t } else { a.utf8 },
                text.as_bytes().to_vec(),
            ),
        );
    }
    for s in (1..=3).rev() {
        println!("Pasting in {s}... click into a text field now.");
        std::thread::sleep(Duration::from_secs(1));
    }
    conn.set_selection_owner(win, a.clipboard, CURRENT_TIME)?;
    let now_owner = conn.get_selection_owner(a.clipboard)?.reply()?.owner;
    anyhow::ensure!(now_owner == win, "could not take the clipboard");
    let focus = conn.get_input_focus()?.reply()?.focus;
    println!(
        "Focused window: 0x{focus:x}. Sending {}.",
        if terminal { "Ctrl+Shift+V" } else { "Ctrl+V" }
    );

    // 3. Ctrl(+Shift)+V via XTest.
    let started = Instant::now();
    send_paste(&conn, root, terminal)?;

    // 4. Serve requests until the app has read our text, then restore.
    let mut served_text = false;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut served_at = None;
    while Instant::now() < deadline {
        if let Some(ev) = conn.poll_for_event()? {
            if let Event::SelectionRequest(req) = ev {
                let name = atom_name(&conn, req.target);
                let ok = serve(&conn, &req, &ours, &a)?;
                println!(
                    "[{:>4} ms] app asked for {name}: {}",
                    started.elapsed().as_millis(),
                    if ok { "served" } else { "refused" }
                );
                if ok && req.target != a.targets {
                    served_text = true;
                    served_at.get_or_insert_with(Instant::now);
                }
            }
        } else {
            std::thread::sleep(Duration::from_millis(2));
        }
        // Some apps ask more than once; give them 150 ms after the first read.
        if served_at.is_some_and(|t| t.elapsed() > Duration::from_millis(150)) {
            break;
        }
    }
    if served_text {
        println!(
            "Text was read by the app after {} ms.",
            started.elapsed().as_millis()
        );
    } else {
        println!(
            "WARNING: nobody read our text within 3 s. Paste key not delivered, or field not focused."
        );
    }

    if saved.is_empty() {
        println!("Nothing to restore. RESULT served_text={served_text} restored=empty");
        return Ok(());
    }
    conn.set_selection_owner(win, a.clipboard, CURRENT_TIME)?;
    conn.flush()?;
    println!("Old clipboard restored. Serving it for {hold} s; paste by hand to check.");
    let end = Instant::now() + Duration::from_secs(hold);
    let mut restored_reads = 0;
    while Instant::now() < end {
        match conn.poll_for_event()? {
            Some(Event::SelectionRequest(req)) => {
                if serve(&conn, &req, &saved, &a)? && req.target != a.targets {
                    restored_reads += 1;
                }
            }
            Some(Event::SelectionClear(_)) => {
                println!(
                    "Someone copied something new; stop serving (that's the right behaviour)."
                );
                break;
            }
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    println!(
        "RESULT served_text={served_text} restored_targets={} restored_reads={restored_reads}",
        saved.len()
    );
    Ok(())
}

fn send_paste(conn: &RustConnection, root: u32, terminal: bool) -> Result<()> {
    let setup = conn.setup();
    let (min, max) = (setup.min_keycode, setup.max_keycode);
    let map = conn.get_keyboard_mapping(min, max - min + 1)?.reply()?;
    let per = map.keysyms_per_keycode as usize;
    let code = |sym: u32| -> Result<u8> {
        map.keysyms
            .chunks(per)
            .position(|s| s.contains(&sym))
            .map(|i| min + i as u8)
            .with_context(|| format!("no keycode for keysym 0x{sym:x}"))
    };
    let ctrl = code(0xffe3)?;
    let shift = code(0xffe1)?;
    let v = code(0x76)?;
    let mut keys = vec![ctrl];
    if terminal {
        keys.push(shift);
    }
    keys.push(v);
    const PRESS: u8 = 2;
    const RELEASE: u8 = 3;
    for &k in &keys {
        conn.xtest_fake_input(PRESS, k, CURRENT_TIME, root, 0, 0, 0)?;
    }
    for &k in keys.iter().rev() {
        conn.xtest_fake_input(RELEASE, k, CURRENT_TIME, root, 0, 0, 0)?;
    }
    conn.flush()?;
    Ok(())
}

fn serve(
    conn: &RustConnection,
    req: &SelectionRequestEvent,
    offer: &Offer,
    a: &Atoms,
) -> Result<bool> {
    let property = if req.property == x11rb::NONE {
        req.target
    } else {
        req.property
    };
    let ok = if req.target == a.targets {
        let mut list: Vec<Atom> = vec![a.targets, a.timestamp];
        list.extend(
            offer
                .keys()
                .copied()
                .filter(|t| *t != a.targets && *t != a.timestamp),
        );
        conn.change_property32(
            PropMode::REPLACE,
            req.requestor,
            property,
            AtomEnum::ATOM,
            &list,
        )?;
        true
    } else if let Some((ty, data)) = offer.get(&req.target) {
        // Big payloads need the INCR protocol; the spike only reports them.
        conn.change_property8(PropMode::REPLACE, req.requestor, property, *ty, data)?;
        true
    } else {
        false
    };
    let ev = SelectionNotifyEvent {
        response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
        sequence: 0,
        time: req.time,
        requestor: req.requestor,
        selection: req.selection,
        target: req.target,
        property: if ok { property } else { x11rb::NONE },
    };
    conn.send_event(false, req.requestor, EventMask::NO_EVENT, ev)?;
    conn.flush()?;
    Ok(ok)
}

/// Read one target of the clipboard into our window's property.
fn fetch(
    conn: &RustConnection,
    win: u32,
    a: &Atoms,
    target: Atom,
) -> Result<Option<(Atom, Vec<u8>, u8)>> {
    conn.convert_selection(win, a.clipboard, target, a.prop, CURRENT_TIME)?;
    conn.flush()?;
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        match conn.poll_for_event()? {
            Some(Event::SelectionNotify(n)) if n.target == target => {
                if n.property == x11rb::NONE {
                    return Ok(None);
                }
                let r = conn
                    .get_property(true, win, a.prop, AtomEnum::ANY, 0, u32::MAX / 4)?
                    .reply()?;
                if r.type_ == a.incr {
                    println!(
                        "  ({} is large and needs INCR; not handled in this spike)",
                        atom_name(conn, target)
                    );
                    return Ok(None);
                }
                return Ok(Some((r.type_, r.value, r.format)));
            }
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(2)),
        }
    }
    Ok(None)
}

fn save_clipboard(conn: &RustConnection, win: u32, a: &Atoms) -> Result<Offer> {
    let mut saved = Offer::new();
    let Some((_, raw, 32)) = fetch(conn, win, a, a.targets)? else {
        println!("Clipboard owner did not answer TARGETS.");
        return Ok(saved);
    };
    let targets: Vec<Atom> = raw
        .chunks_exact(4)
        .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    for t in targets {
        if [a.targets, a.timestamp, a.multiple, a.save_targets].contains(&t) {
            continue;
        }
        if let Some((ty, data, format)) = fetch(conn, win, a, t)? {
            if format == 8 {
                saved.insert(t, (ty, data));
            }
        }
    }
    Ok(saved)
}
