//! The floating pill shown while dictating. It never takes keyboard focus: on X11 it is an
//! override-redirect window (the window manager ignores it), which the M0 spike showed is the
//! only approach that never steals focus on Cinnamon.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk4::prelude::*;
use gtk4::{cairo, gdk};
use vaani_core::state::State;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ConfigureWindowAux, ConnectionExt as _, PropMode,
    StackMode,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use crate::controller::{EngineStatus, Shared};
use crate::style::pill_colors;

const W: i32 = 232;
const H: i32 = 44;
const BARS: usize = 22;
const MARGIN_BOTTOM: i32 = 72;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Hidden,
    Recording,
    Processing,
    Done,
}

struct Anim {
    levels: VecDeque<f32>,
    phase: f32,
    mode: Mode,
}

pub struct Overlay {
    shared: Arc<Shared>,
    window: gtk4::Window,
    label: gtk4::Label,
    hint: gtk4::Label,
    area: gtk4::DrawingArea,
    anim: Rc<RefCell<Anim>>,
    xconn: Option<Rc<RustConnection>>,
    xid: Rc<Cell<u32>>,
    shown_done_for: RefCell<Option<Instant>>,
}

impl Overlay {
    pub fn new(shared: Arc<Shared>) -> Self {
        let window = gtk4::Window::builder()
            .decorated(false)
            .resizable(false)
            .default_width(W)
            .default_height(H)
            .focusable(false)
            .can_focus(false)
            .css_classes(["vaani-overlay"])
            .title("Vaani")
            .build();
        let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
        row.add_css_class("vaani-pill");
        row.set_size_request(W, H);
        let area = gtk4::DrawingArea::builder()
            .content_width(132)
            .content_height(H)
            .hexpand(true)
            .build();
        let label = gtk4::Label::new(Some("0:00"));
        let hint = gtk4::Label::new(None);
        hint.add_css_class("dim");
        hint.set_visible(false);
        row.append(&area);
        row.append(&hint);
        row.append(&label);
        window.set_child(Some(&row));

        let anim = Rc::new(RefCell::new(Anim {
            levels: VecDeque::from(vec![0.0; BARS]),
            phase: 0.0,
            mode: Mode::Hidden,
        }));
        {
            let anim = anim.clone();
            area.set_draw_func(move |_, cr, w, h| draw(cr, w, h, &anim.borrow()));
        }

        let xconn = x11rb::connect(None).ok().map(|(c, _)| Rc::new(c));
        let xid = Rc::new(Cell::new(0u32));
        {
            let xconn = xconn.clone();
            let xid = xid.clone();
            window.connect_realize(move |w| {
                let Some(surface) = w.surface().and_downcast::<gdk4_x11::X11Surface>() else {
                    tracing::warn!("overlay: not an X11 surface; it may take focus");
                    return;
                };
                let id = surface.xid() as u32;
                xid.set(id);
                if let Some(c) = &xconn {
                    make_unfocusable(c, id);
                }
            });
        }
        {
            let xconn = xconn.clone();
            let xid = xid.clone();
            window.connect_map(move |w| {
                if let Some(c) = &xconn {
                    make_unfocusable(c, xid.get());
                    place(c, w, xid.get());
                }
            });
        }

        Self {
            shared,
            window,
            label,
            hint,
            area,
            anim,
            xconn,
            xid,
            shown_done_for: RefCell::new(None),
        }
    }

    pub fn visible(&self) -> bool {
        self.window.is_visible()
    }

    /// Called about 30 times a second from the GTK main loop.
    pub fn tick(&self) {
        let enabled = self
            .shared
            .config
            .lock()
            .map(|c| c.general.overlay)
            .unwrap_or(true);
        let state = self.shared.state();
        let just_done = self
            .shared
            .last_text
            .lock()
            .ok()
            .and_then(|l| l.as_ref().map(|(t, _)| *t))
            .filter(|t| t.elapsed() < Duration::from_millis(700));
        let mode = match state {
            State::Recording => Mode::Recording,
            State::Processing => Mode::Processing,
            State::Idle if just_done.is_some() => Mode::Done,
            State::Idle => Mode::Hidden,
        };
        let mode = if enabled { mode } else { Mode::Hidden };
        let _ = &self.shown_done_for;

        {
            let mut a = self.anim.borrow_mut();
            a.phase += 0.033;
            if mode == Mode::Recording {
                let lvl = self.shared.level.get();
                // Speech RMS is small; map roughly -50..-10 dBFS onto 0..1.
                let db = 20.0 * lvl.max(1e-6).log10();
                let v = ((db + 50.0) / 40.0).clamp(0.0, 1.0);
                a.levels.pop_front();
                a.levels.push_back(v);
            } else if a.mode == Mode::Recording {
                a.levels.iter_mut().for_each(|l| *l = 0.0);
            }
            if a.mode != mode && mode == Mode::Recording {
                a.levels.iter_mut().for_each(|l| *l = 0.0);
            }
            a.mode = mode;
        }

        match mode {
            Mode::Hidden => {
                if self.window.is_visible() {
                    self.window.set_visible(false);
                }
                return;
            }
            Mode::Recording => {
                let secs = self
                    .shared
                    .recording_since
                    .lock()
                    .ok()
                    .and_then(|t| t.map(|t| t.elapsed().as_secs()))
                    .unwrap_or(0);
                self.label
                    .set_text(&format!("{}:{:02}", secs / 60, secs % 60));
                let hands_free = self
                    .shared
                    .hands_free
                    .load(std::sync::atomic::Ordering::Relaxed);
                let engine = self.shared.engine();
                let loading = matches!(engine, EngineStatus::Loading(_));
                let cloud = matches!(&engine, EngineStatus::Ready(n) if n.ends_with("(cloud)"));
                if loading {
                    self.hint.set_text("loading model");
                    self.hint.set_visible(true);
                } else if hands_free {
                    self.hint.set_text(if cloud {
                        "☁ tap to finish"
                    } else {
                        "tap to finish"
                    });
                    self.hint.set_visible(true);
                } else if cloud {
                    // Privacy is visible: audio is leaving the machine.
                    self.hint.set_text("☁ cloud");
                    self.hint.set_visible(true);
                } else {
                    self.hint.set_visible(false);
                }
            }
            Mode::Processing => {
                self.label.set_text("thinking…");
                self.hint.set_visible(false);
            }
            Mode::Done => {
                self.label.set_text("done");
                self.hint.set_visible(false);
            }
        }
        if !self.window.is_visible() {
            self.window.set_visible(true);
        } else if let Some(c) = &self.xconn {
            // Keep it on top of windows that were raised meanwhile.
            let _ = c.configure_window(
                self.xid.get(),
                &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
            );
            let _ = c.flush();
        }
        self.area.queue_draw();
    }
}

fn make_unfocusable(c: &RustConnection, id: u32) {
    if id == 0 {
        return;
    }
    let wm_hints: u32 = AtomEnum::WM_HINTS.into();
    // flags = InputHint, input = False
    let _ = c.change_property32(
        PropMode::REPLACE,
        id,
        wm_hints,
        wm_hints,
        &[1, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    let _ = c.change_window_attributes(id, &ChangeWindowAttributesAux::new().override_redirect(1));
    let _ = c.flush();
}

/// Bottom centre of the monitor the mouse pointer is on.
fn place(c: &RustConnection, w: &gtk4::Window, id: u32) {
    if id == 0 {
        return;
    }
    let root = c.setup().roots.first().map(|s| s.root).unwrap_or(0);
    let (px, py) = c
        .query_pointer(root)
        .ok()
        .and_then(|r| r.reply().ok())
        .map(|r| (i32::from(r.root_x), i32::from(r.root_y)))
        .unwrap_or((0, 0));
    let scale = w.scale_factor().max(1);
    let display = gtk4::prelude::WidgetExt::display(w);
    let monitors = display.monitors();
    let mut geo = None;
    for i in 0..monitors.n_items() {
        if let Some(m) = monitors.item(i).and_downcast::<gdk::Monitor>() {
            let g = m.geometry();
            let (x, y, ww, hh) = (
                g.x() * scale,
                g.y() * scale,
                g.width() * scale,
                g.height() * scale,
            );
            if geo.is_none() || (px >= x && px < x + ww && py >= y && py < y + hh) {
                geo = Some((x, y, ww, hh));
                if px >= x && px < x + ww && py >= y && py < y + hh {
                    break;
                }
            }
        }
    }
    let (mx, my, mw, mh) = geo.unwrap_or((0, 0, 1920, 1080));
    let x = mx + (mw - W * scale) / 2;
    let y = my + mh - (H + MARGIN_BOTTOM) * scale;
    let _ = c.configure_window(
        id,
        &ConfigureWindowAux::new()
            .x(x)
            .y(y)
            .stack_mode(StackMode::ABOVE),
    );
    let _ = c.flush();
}

fn rounded(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -std::f64::consts::FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, std::f64::consts::FRAC_PI_2);
    cr.arc(
        x + r,
        y + h - r,
        r,
        std::f64::consts::FRAC_PI_2,
        std::f64::consts::PI,
    );
    cr.arc(
        x + r,
        y + r,
        r,
        std::f64::consts::PI,
        1.5 * std::f64::consts::PI,
    );
    cr.close_path();
}

fn draw(cr: &cairo::Context, w: i32, h: i32, a: &Anim) {
    let (w, h) = (f64::from(w), f64::from(h));
    let cy = h / 2.0;
    match a.mode {
        Mode::Recording => {
            // Pulsing red dot.
            let pulse = 0.75 + 0.25 * (a.phase as f64 * 5.0).sin();
            let (r, g, b) = pill_colors().dot;
            cr.set_source_rgba(r, g, b, pulse);
            cr.arc(7.0, cy, 5.0, 0.0, std::f64::consts::TAU);
            let _ = cr.fill();
            // Level bars, newest on the right.
            let x0 = 22.0;
            let span = w - x0 - 2.0;
            let step = span / BARS as f64;
            let bw = (step * 0.55).max(2.0);
            for (i, l) in a.levels.iter().enumerate() {
                let bh = 4.0 + f64::from(*l) * (h - 18.0);
                let x = x0 + i as f64 * step;
                let t = i as f64 / (BARS - 1) as f64;
                let (r, g, b) = gradient(t);
                cr.set_source_rgba(r, g, b, 0.55 + 0.45 * t);
                rounded(cr, x, cy - bh / 2.0, bw, bh, bw / 2.0);
                let _ = cr.fill();
            }
        }
        Mode::Processing => {
            // Three dots in a wave.
            for i in 0..3 {
                let t = a.phase as f64 * 6.0 - i as f64 * 0.7;
                let lift = (t.sin().max(0.0)) * 5.0;
                let (r, g, b) = gradient(i as f64 / 2.0);
                cr.set_source_rgba(r, g, b, 1.0);
                cr.arc(
                    10.0 + i as f64 * 14.0,
                    cy - lift,
                    4.0,
                    0.0,
                    std::f64::consts::TAU,
                );
                let _ = cr.fill();
            }
        }
        Mode::Done => {
            // Check mark.
            let (r, g, b) = pill_colors().done;
            cr.set_source_rgba(r, g, b, 1.0);
            cr.set_line_width(3.0);
            cr.set_line_cap(cairo::LineCap::Round);
            cr.move_to(4.0, cy);
            cr.line_to(10.0, cy + 6.0);
            cr.line_to(21.0, cy - 7.0);
            let _ = cr.stroke();
        }
        Mode::Hidden => {}
    }
}

/// Rose → pink → orchid, t in 0..=1.
fn gradient(t: f64) -> (f64, f64, f64) {
    let lerp = |a: (f64, f64, f64), b: (f64, f64, f64), t: f64| {
        (
            a.0 + (b.0 - a.0) * t,
            a.1 + (b.1 - a.1) * t,
            a.2 + (b.2 - a.2) * t,
        )
    };
    let [a, b, c] = pill_colors().bars;
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        lerp(a, b, t * 2.0)
    } else {
        lerp(b, c, (t - 0.5) * 2.0)
    }
}
