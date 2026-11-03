//! Linux (X11 and XWayland): window placement the pill and the settings window need, which
//! GTK 4 has no API for. The pill is an override-redirect window, which the M0 spike showed
//! is the only approach that never steals focus on Cinnamon.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gtk4::prelude::*;
use gtk4::{cairo, gdk};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ConfigureWindowAux,
    ConnectionExt as _, EventMask, PropMode, StackMode,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// The pill's native window: kept unfocusable, on top and at the bottom centre of the
/// monitor the pointer was on when it appeared.
pub struct Pill {
    xconn: Option<Rc<RustConnection>>,
    xid: Rc<Cell<u32>>,
    /// The monitor the pill went to when it appeared, and the window size it was placed for.
    monitor: Rc<Cell<(i32, i32, i32, i32)>>,
    placed_size: Cell<(i32, i32)>,
}

impl Pill {
    /// Hook into `window`'s realize and map. `size` is the pill's own size in logical pixels.
    pub fn new(window: &gtk4::Window, size: (i32, i32), margin_bottom: i32) -> Self {
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
                // Nothing in the overlay is clickable: let clicks through to the windows below.
                surface.set_input_region(Some(&cairo::Region::create()));
                let id = surface.xid() as u32;
                xid.set(id);
                if let Some(c) = &xconn {
                    make_unfocusable(c, id);
                }
            });
        }
        let monitor = Rc::new(Cell::new((0, 0, 1920, 1080)));
        {
            let xconn = xconn.clone();
            let xid = xid.clone();
            let monitor = monitor.clone();
            window.connect_map(move |w| {
                if let Some(c) = &xconn {
                    make_unfocusable(c, xid.get());
                    monitor.set(pointer_monitor(c, w));
                    let scale = w.scale_factor().max(1);
                    place(
                        c,
                        xid.get(),
                        monitor.get(),
                        (size.0 * scale, size.1 * scale),
                        scale,
                        margin_bottom,
                    );
                }
            });
        }
        Self {
            xconn,
            xid,
            monitor,
            placed_size: Cell::new((0, 0)),
        }
    }

    pub fn shown(&self, window: &gtk4::Window) -> bool {
        window.is_visible()
    }

    pub fn show(&self, window: &gtk4::Window) {
        window.set_visible(true);
    }

    pub fn hide(&self, window: &gtk4::Window) {
        window.set_visible(false);
    }

    /// While shown: keep it bottom-centred as it grows or shrinks with the caption, and on
    /// top of windows that were raised meanwhile.
    pub fn keep_placed(&self, window: &gtk4::Window, margin_bottom: i32) {
        let Some(c) = &self.xconn else { return };
        let id = self.xid.get();
        let size = c
            .get_geometry(id)
            .ok()
            .and_then(|r| r.reply().ok())
            .map(|g| (i32::from(g.width), i32::from(g.height)));
        if let Some(size) = size.filter(|s| *s != self.placed_size.get()) {
            self.placed_size.set(size);
            place(
                c,
                id,
                self.monitor.get(),
                size,
                window.scale_factor().max(1),
                margin_bottom,
            );
        }
        let _ = c.configure_window(id, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
        let _ = c.flush();
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
    // Hyprland focuses an override-redirect XWayland window when it appears unless its type
    // says it is a notification, tooltip or similar; the pill would then take the keys meant
    // for the app.
    let atom = |name: &[u8]| {
        c.intern_atom(false, name)
            .ok()
            .and_then(|r| r.reply().ok())
            .map(|r| r.atom)
    };
    if let (Some(prop), Some(kind)) = (
        atom(b"_NET_WM_WINDOW_TYPE"),
        atom(b"_NET_WM_WINDOW_TYPE_NOTIFICATION"),
    ) {
        let _ = c.change_property32(PropMode::REPLACE, id, prop, AtomEnum::ATOM, &[kind]);
    }
    let _ = c.change_window_attributes(id, &ChangeWindowAttributesAux::new().override_redirect(1));
    let _ = c.flush();
}

/// Bottom centre of `monitor` (x, y, width, height) for a window of `size`, in device pixels.
fn place(
    c: &RustConnection,
    id: u32,
    monitor: (i32, i32, i32, i32),
    size: (i32, i32),
    scale: i32,
    margin_bottom: i32,
) {
    if id == 0 {
        return;
    }
    let (mx, my, mw, mh) = monitor;
    let x = mx + (mw - size.0) / 2;
    let y = my + mh - size.1 - margin_bottom * scale;
    let _ = c.configure_window(
        id,
        &ConfigureWindowAux::new()
            .x(x)
            .y(y)
            .stack_mode(StackMode::ABOVE),
    );
    let _ = c.flush();
}

/// Geometry (x, y, width, height in device pixels) of the monitor the mouse pointer is on.
pub fn pointer_monitor(c: &RustConnection, w: &gtk4::Window) -> (i32, i32, i32, i32) {
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
    geo.unwrap_or((0, 0, 1920, 1080))
}

/// Move a normal (window-manager managed) window to the centre of the monitor under the
/// pointer. GTK 4 has no API for this on X11, and some window managers (Cinnamon among them)
/// otherwise place new windows at the top left.
/// Returns true when the window already sits where it should.
pub fn center_window(w: &gtk4::Window) -> bool {
    let Some(surface) = w.surface().and_downcast::<gdk4_x11::X11Surface>() else {
        return true;
    };
    let id = surface.xid() as u32;
    let Ok((c, _)) = x11rb::connect(None) else {
        return true;
    };
    let scale = w.scale_factor().max(1);
    // The X window includes the shadow GTK draws around client-side decorations, so use its
    // real size rather than the widget size.
    let (ww, wh) = c
        .get_geometry(id)
        .ok()
        .and_then(|r| r.reply().ok())
        .filter(|g| g.width > 1)
        .map(|g| (i32::from(g.width), i32::from(g.height)))
        .unwrap_or((
            w.width().max(w.default_width()) * scale,
            w.height().max(w.default_height()) * scale,
        ));
    let (mx, my, mw, mh) = pointer_monitor(&c, w);
    let x = mx + ((mw - ww) / 2).max(0);
    let y = my + ((mh - wh) / 2).max(0);
    let root = c.setup().roots.first().map(|s| s.root).unwrap_or(0);
    let now = c
        .translate_coordinates(id, root, 0, 0)
        .ok()
        .and_then(|r| r.reply().ok())
        .map(|r| (i32::from(r.dst_x), i32::from(r.dst_y)));
    if now == Some((x, y)) {
        return true;
    }
    tracing::debug!(x, y, ww, wh, ?now, "centring settings window");
    // Ask the window manager through EWMH (honoured by Muffin, Mutter, KWin, Xfwm, Openbox),
    // and move the window directly too for window managers without it.
    if let Ok(atom) = c
        .intern_atom(false, b"_NET_MOVERESIZE_WINDOW")
        .map_err(|_| ())
        .and_then(|r| r.reply().map_err(|_| ()))
    {
        // Static gravity, x and y given, request from a pager/user (source 2).
        let flags: u32 = 10 | (1 << 8) | (1 << 9) | (2 << 12);
        let ev = ClientMessageEvent::new(32, id, atom.atom, [flags, x as u32, y as u32, 0, 0]);
        let _ = c.send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            ev,
        );
    }
    let _ = c.configure_window(id, &ConfigureWindowAux::new().x(x).y(y));
    let _ = c.flush();
    false
}

/// Centre `w` when it is shown, and once more after the window manager has placed it.
pub fn keep_centered_on_open(w: &gtk4::Window) {
    w.connect_map(|w| {
        // Window managers place a new window some time after it maps (Cinnamon can take a
        // second), so keep putting it in the centre until it has stayed there for a moment.
        let w = w.clone();
        let (mut tries, mut settled) = (0u32, 0u32);
        gtk4::glib::timeout_add_local(Duration::from_millis(100), move || {
            tries += 1;
            if center_window(&w) {
                settled += 1;
            } else {
                settled = 0;
            }
            if settled >= 5 || tries >= 30 || !w.is_visible() {
                gtk4::glib::ControlFlow::Break
            } else {
                gtk4::glib::ControlFlow::Continue
            }
        });
    });
}
