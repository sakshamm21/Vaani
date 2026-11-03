//! Windows: window placement the pill and the settings window need, which GTK 4 has no API
//! for.
//!
//! The pill is a tool window that never activates and stays topmost. Showing a GTK window
//! activates it, which would take the keyboard away from the app being dictated into, so the
//! pill is shown once, off screen, and then only moved on and off screen without activation.

#![allow(unsafe_code)]

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gtk4::prelude::*;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetCursorPos, GetWindowLongPtrW, GetWindowRect, HWND_TOPMOST, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SetWindowLongPtrW, SetWindowPos,
    WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
};

/// Where the pill waits while hidden.
const AWAY: i32 = -32000;

fn hwnd_of(w: &gtk4::Window) -> Option<HWND> {
    let surface = w.surface()?.downcast::<gdk4_win32::Win32Surface>().ok()?;
    let h = surface.handle();
    (!h.0.is_null()).then_some(HWND(h.0))
}

/// The work area (screen minus taskbar) of the monitor under the mouse pointer.
fn pointer_work_area() -> RECT {
    let mut p = POINT::default();
    let mut mi = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: plain queries into stack structs.
    unsafe {
        let _ = GetCursorPos(&mut p);
        let mon = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            return mi.rcWork;
        }
    }
    RECT {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1040,
    }
}

fn window_rect(h: HWND) -> Option<RECT> {
    let mut r = RECT::default();
    // SAFETY: plain query.
    unsafe { GetWindowRect(h, &mut r) }.ok().map(|_| r)
}

fn scale(h: HWND) -> f64 {
    // SAFETY: plain query; 0 means unknown.
    let dpi = unsafe { GetDpiForWindow(h) };
    if dpi == 0 { 1.0 } else { f64::from(dpi) / 96.0 }
}

fn move_to(h: HWND, x: i32, y: i32, show: bool) {
    let mut flags = SWP_NOSIZE | SWP_NOACTIVATE;
    if show {
        flags |= SWP_SHOWWINDOW;
    }
    // SAFETY: repositioning our own window.
    let _ = unsafe { SetWindowPos(h, Some(HWND_TOPMOST), x, y, 0, 0, flags) };
}

/// The pill's native window.
pub struct Pill {
    hwnd: Rc<Cell<isize>>,
    margin_bottom: i32,
    on_screen: Cell<bool>,
    placed: Cell<(i32, i32, i32, i32)>,
}

impl Pill {
    pub fn new(window: &gtk4::Window, _size: (i32, i32), margin_bottom: i32) -> Self {
        let hwnd = Rc::new(Cell::new(0isize));
        {
            let hwnd = hwnd.clone();
            window.connect_realize(move |w| {
                let Some(h) = hwnd_of(w) else {
                    tracing::warn!("overlay: no native window; it may take focus");
                    return;
                };
                hwnd.set(h.0 as isize);
                // SAFETY: adjusting the extended style of our own window: never activated by
                // clicks, no taskbar button, always on top.
                unsafe {
                    let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
                    let ex = (ex & !(WS_EX_APPWINDOW.0 as isize))
                        | (WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0 | WS_EX_TOPMOST.0) as isize;
                    SetWindowLongPtrW(h, GWL_EXSTYLE, ex);
                }
                move_to(h, AWAY, AWAY, false);
            });
        }
        Self {
            hwnd,
            margin_bottom,
            on_screen: Cell::new(false),
            placed: Cell::new((0, 0, 0, 0)),
        }
    }

    fn handle(&self) -> Option<HWND> {
        let h = self.hwnd.get();
        (h != 0).then_some(HWND(h as *mut _))
    }

    pub fn shown(&self, _window: &gtk4::Window) -> bool {
        self.on_screen.get()
    }

    pub fn show(&self, window: &gtk4::Window) {
        if !window.is_visible() {
            // First time: GTK maps it (off screen, see realize).
            window.set_visible(true);
        }
        self.on_screen.set(true);
        self.placed.set((0, 0, 0, 0));
        self.keep_placed(window, self.margin_bottom);
    }

    pub fn hide(&self, _window: &gtk4::Window) {
        self.on_screen.set(false);
        if let Some(h) = self.handle() {
            move_to(h, AWAY, AWAY, false);
        }
    }

    /// Bottom centre of the work area under the pointer; again whenever the window grows or
    /// shrinks with the caption. Topmost each time, above windows raised meanwhile.
    pub fn keep_placed(&self, _window: &gtk4::Window, margin_bottom: i32) {
        let Some(h) = self.handle() else { return };
        if !self.on_screen.get() {
            return;
        }
        let Some(r) = window_rect(h) else { return };
        let (w, ht) = (r.right - r.left, r.bottom - r.top);
        let first = self.placed.get() == (0, 0, 0, 0);
        let area = if first {
            pointer_work_area()
        } else {
            let (l, t, rr, b) = self.placed.get();
            RECT {
                left: l,
                top: t,
                right: rr,
                bottom: b,
            }
        };
        self.placed
            .set((area.left, area.top, area.right, area.bottom));
        let margin = (f64::from(margin_bottom) * scale(h)).round() as i32;
        let x = area.left + ((area.right - area.left) - w) / 2;
        let y = area.bottom - ht - margin;
        if first || (r.left, r.top) != (x, y) {
            move_to(h, x, y, true);
        } else {
            // SAFETY: keep it above windows raised since; no move, no activation.
            let _ = unsafe {
                SetWindowPos(
                    h,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOMOVE,
                )
            };
        }
    }
}

/// Move a normal window to the centre of the work area under the pointer. Returns true when
/// it already sits there.
pub fn center_window(w: &gtk4::Window) -> bool {
    let Some(h) = hwnd_of(w) else { return true };
    let Some(r) = window_rect(h) else { return true };
    let (ww, wh) = (r.right - r.left, r.bottom - r.top);
    if ww <= 1 {
        return false;
    }
    let a = pointer_work_area();
    let x = a.left + ((a.right - a.left - ww) / 2).max(0);
    let y = a.top + ((a.bottom - a.top - wh) / 2).max(0);
    if (r.left, r.top) == (x, y) {
        return true;
    }
    // SAFETY: repositioning our own window, keeping its size and z-order.
    let _ = unsafe { SetWindowPos(h, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER) };
    false
}

/// Centre `w` when it is shown, and keep it there while GTK finishes sizing it.
pub fn keep_centered_on_open(w: &gtk4::Window) {
    w.connect_map(|w| {
        let w = w.clone();
        let (mut tries, mut settled) = (0u32, 0u32);
        gtk4::glib::timeout_add_local(Duration::from_millis(50), move || {
            tries += 1;
            if center_window(&w) {
                settled += 1;
            } else {
                settled = 0;
            }
            if settled >= 3 || tries >= 20 || !w.is_visible() {
                gtk4::glib::ControlFlow::Break
            } else {
                gtk4::glib::ControlFlow::Continue
            }
        });
    });
}
