//! Spike: a floating "listening" pill on X11 that never takes keyboard focus.
//!
//! Question: can a GTK4 window float above everything on Cinnamon without stealing focus from
//! the app the user is typing into? Two approaches are compared:
//!   --mode override  (default) X11 override-redirect: the window manager ignores the window.
//!   --mode hints     a normal managed window with "notification" type and input=false hints.
//!
//! Run:   cargo run -- [--mode override|hints] [--seconds N]
//!        Click into a text editor first, start the spike from a terminal with a delay, e.g.
//!        `sleep 3; cargo run`, then keep typing while the pill is shown. Your typing should
//!        keep landing in the editor, and the report should say the focus never changed.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ConfigureWindowAux, ConnectionExt as _, PropMode,
    StackMode,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

const W: u32 = 240;
const H: u32 = 48;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Override,
    Hints,
}

fn atom(c: &RustConnection, n: &str) -> Atom {
    c.intern_atom(false, n.as_bytes())
        .ok()
        .and_then(|r| r.reply().ok())
        .map(|r| r.atom)
        .unwrap_or(0)
}

/// (keyboard focus window, _NET_ACTIVE_WINDOW)
fn focus_state(c: &RustConnection, root: u32, net_active: Atom) -> (u32, u32) {
    let focus = c
        .get_input_focus()
        .ok()
        .and_then(|r| r.reply().ok())
        .map(|r| r.focus)
        .unwrap_or(0);
    let active = c
        .get_property(false, root, net_active, AtomEnum::WINDOW, 0, 1)
        .ok()
        .and_then(|r| r.reply().ok())
        .and_then(|r| r.value32().and_then(|mut v| v.next()))
        .unwrap_or(0);
    (focus, active)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mode = match args
        .iter()
        .position(|a| a == "--mode")
        .and_then(|i| args.get(i + 1))
    {
        Some(m) if m == "hints" => Mode::Hints,
        _ => Mode::Override,
    };
    let seconds: u64 = args
        .iter()
        .position(|a| a == "--seconds")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(6);

    // Force the X11 backend for this spike.
    if std::env::var_os("GDK_BACKEND").is_none() {
        // SAFETY: set_var is only unsafe with other threads running; none exist yet.
        unsafe { std::env::set_var("GDK_BACKEND", "x11") };
    }

    let (xc, screen_num) = x11rb::connect(None).context("cannot connect to the X server")?;
    let screen = xc.setup().roots[screen_num].clone();
    let root = screen.root;
    let net_active = atom(&xc, "_NET_ACTIVE_WINDOW");
    let before = focus_state(&xc, root, net_active);
    println!(
        "Mode: {}. Focus before: window 0x{:x}, active 0x{:x}",
        if mode == Mode::Override {
            "override-redirect"
        } else {
            "WM hints"
        },
        before.0,
        before.1
    );
    let xc = Rc::new(xc);

    gtk4::init()?;
    let css = gtk4::CssProvider::new();
    css.load_from_data(
        "window.vaani-pill { background: transparent; }
         .pill { background: rgba(20,20,24,0.92); color: white; border-radius: 24px; padding: 8px 18px; }
         levelbar block.filled { background: #4ade80; }",
    );
    if let Some(display) = gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &css,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    let window = gtk4::Window::builder()
        .decorated(false)
        .resizable(false)
        .default_width(W as i32)
        .default_height(H as i32)
        .focusable(false)
        .can_focus(false)
        .css_classes(["vaani-pill"])
        .build();
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    row.add_css_class("pill");
    let label = gtk4::Label::new(Some("● Listening"));
    let level = gtk4::LevelBar::builder()
        .min_value(0.0)
        .max_value(1.0)
        .hexpand(true)
        .build();
    row.append(&label);
    row.append(&level);
    window.set_child(Some(&row));

    let xid = Rc::new(Cell::new(0u32));
    {
        let xc = xc.clone();
        let xid = xid.clone();
        window.connect_realize(move |w| {
            let Some(surface) = w.surface().and_downcast::<gdk4_x11::X11Surface>() else {
                eprintln!("not an X11 surface; is GDK_BACKEND=x11?");
                return;
            };
            let id = surface.xid() as u32;
            xid.set(id);
            prepare(&xc, id, mode);
        });
    }
    {
        let xc = xc.clone();
        let xid = xid.clone();
        let screen = screen.clone();
        window.connect_map(move |_| {
            let id = xid.get();
            prepare(&xc, id, mode);
            // Bottom centre, 64 px above the screen edge.
            let x = (i32::from(screen.width_in_pixels) - W as i32) / 2;
            let y = i32::from(screen.height_in_pixels) - H as i32 - 64;
            let _ = xc.configure_window(
                id,
                &ConfigureWindowAux::new()
                    .x(x)
                    .y(y)
                    .stack_mode(StackMode::ABOVE),
            );
            let _ = xc.flush();
        });
    }

    let main_loop = glib::MainLoop::new(None, false);
    let start = Instant::now();
    let changes = Rc::new(Cell::new(0u32));
    window.present();
    {
        let xc = xc.clone();
        let changes = changes.clone();
        let main_loop = main_loop.clone();
        let window = window.clone();
        let xid = xid.clone();
        let mut phase = 0f64;
        glib::timeout_add_local(Duration::from_millis(50), move || {
            phase += 0.25;
            level.set_value((phase.sin() * 0.5 + 0.5).clamp(0.0, 1.0));
            let now = focus_state(&xc, root, net_active);
            if now != before && changes.get() == 0 {
                println!(
                    "[{:>5} ms] FOCUS CHANGED to window 0x{:x}, active 0x{:x} (overlay is 0x{:x})",
                    start.elapsed().as_millis(),
                    now.0,
                    now.1,
                    xid.get()
                );
                changes.set(changes.get() + 1);
            }
            if start.elapsed() > Duration::from_secs(seconds) {
                window.close();
                main_loop.quit();
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }
    main_loop.run();
    let after = focus_state(&xc, root, net_active);
    println!(
        "Focus after: window 0x{:x}, active 0x{:x}",
        after.0, after.1
    );
    println!(
        "RESULT focus_stolen={} overlay_xid=0x{:x}",
        changes.get() > 0 || after != before,
        xid.get()
    );
    Ok(())
}

/// Make the window unfocusable before (and again after) it is mapped.
fn prepare(xc: &RustConnection, id: u32, mode: Mode) {
    if id == 0 {
        return;
    }
    // WM_HINTS: flags = InputHint (1), input = False.
    let wm_hints = atom(xc, "WM_HINTS");
    let _ = xc.change_property32(
        PropMode::REPLACE,
        id,
        wm_hints,
        wm_hints,
        &[1, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    match mode {
        Mode::Override => {
            let _ = xc.change_window_attributes(
                id,
                &ChangeWindowAttributesAux::new().override_redirect(1),
            );
        }
        Mode::Hints => {
            let ty = atom(xc, "_NET_WM_WINDOW_TYPE");
            let notif = atom(xc, "_NET_WM_WINDOW_TYPE_NOTIFICATION");
            let _ = xc.change_property32(PropMode::REPLACE, id, ty, AtomEnum::ATOM, &[notif]);
            let state = atom(xc, "_NET_WM_STATE");
            let vals = [
                atom(xc, "_NET_WM_STATE_ABOVE"),
                atom(xc, "_NET_WM_STATE_SKIP_TASKBAR"),
                atom(xc, "_NET_WM_STATE_SKIP_PAGER"),
            ];
            let _ = xc.change_property32(PropMode::REPLACE, id, state, AtomEnum::ATOM, &vals);
        }
    }
    let _ = xc.flush();
}
