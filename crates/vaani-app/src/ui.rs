//! The GTK side of the app: runs on the main thread. Other threads ask it to do things
//! (open settings, quit) through [`request_settings`] and [`request_quit`].

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use gtk4::glib;

use crate::controller::{Input, Shared};

pub enum UiCmd {
    ShowSettings,
    Quit,
}

static UI_TX: OnceLock<Sender<UiCmd>> = OnceLock::new();

pub fn request_settings() {
    if let Some(tx) = UI_TX.get() {
        let _ = tx.send(UiCmd::ShowSettings);
    }
}

pub fn request_quit() {
    if let Some(tx) = UI_TX.get() {
        let _ = tx.send(UiCmd::Quit);
    }
}

pub fn channel() -> Receiver<UiCmd> {
    let (tx, rx) = crossbeam_channel::unbounded();
    let _ = UI_TX.set(tx);
    rx
}

/// Run the GTK main loop until asked to quit.
pub fn run(shared: Arc<Shared>, ctrl: Sender<Input>, rx: Receiver<UiCmd>, show_settings: bool) {
    crate::style::ensure_fonts();
    // A window class of its own, not the app-menu entry's: taskbars such as Cinnamon's then
    // show the window's own icon, which follows Vaani's theme, instead of the menu icon.
    glib::set_prgname(Some("Vaani"));
    glib::set_application_name("Vaani");
    if let Err(e) = gtk4::init() {
        tracing::error!("GTK could not start ({e}); running without overlay and settings");
        // Still block until quit so dictation keeps working.
        while let Ok(cmd) = rx.recv() {
            if matches!(cmd, UiCmd::Quit) {
                return;
            }
        }
        return;
    }
    crate::style::apply(theme_of(&shared));
    let main_loop = glib::MainLoop::new(None, false);
    let overlay = crate::overlay::Overlay::new(shared.clone());
    let settings: Rc<RefCell<Option<crate::settings::Settings>>> = Default::default();
    if show_settings {
        open_settings(&settings, &shared, &ctrl);
    }

    let ctx = Rc::new(Ctx {
        overlay,
        settings,
        shared,
        ctrl,
        rx,
        main_loop: main_loop.clone(),
    });
    schedule(ctx, FAST);
    main_loop.run();
}

fn open_settings(
    slot: &std::rc::Rc<std::cell::RefCell<Option<crate::settings::Settings>>>,
    shared: &Arc<Shared>,
    ctrl: &Sender<Input>,
) {
    let mut s = slot.borrow_mut();
    match s.as_ref() {
        Some(existing) if existing.is_open() => existing.present(),
        _ => *s = Some(crate::settings::Settings::new(shared.clone(), ctrl.clone())),
    }
}

/// Redraw rate while something is animating (pill shown, settings open).
const FAST: Duration = Duration::from_millis(33);
/// Wake-up rate when idle in the tray: just enough to notice a hotkey press quickly.
const IDLE: Duration = Duration::from_millis(100);

struct Ctx {
    overlay: crate::overlay::Overlay,
    settings: Rc<RefCell<Option<crate::settings::Settings>>>,
    shared: Arc<Shared>,
    ctrl: Sender<Input>,
    rx: Receiver<UiCmd>,
    main_loop: glib::MainLoop,
}

/// One timer at a time, re-armed with a delay that depends on whether anything is on
/// screen. Idle, Vaani wakes 10 times a second and does almost nothing.
fn schedule(ctx: Rc<Ctx>, delay: Duration) {
    glib::timeout_add_local_once(delay, move || {
        if let Some(next) = step(&ctx) {
            schedule(ctx, next);
        }
    });
}

fn theme_of(shared: &Shared) -> vaani_core::config::Theme {
    shared
        .config
        .lock()
        .map(|c| c.general.theme)
        .unwrap_or_default()
}

fn step(ctx: &Ctx) -> Option<Duration> {
    crate::style::apply(theme_of(&ctx.shared));
    ctx.overlay.tick();
    while let Ok(cmd) = ctx.rx.try_recv() {
        match cmd {
            UiCmd::ShowSettings => open_settings(&ctx.settings, &ctx.shared, &ctx.ctrl),
            UiCmd::Quit => {
                ctx.main_loop.quit();
                return None;
            }
        }
    }
    let mut slot = ctx.settings.borrow_mut();
    let settings_open = match slot.as_ref() {
        Some(s) if s.is_open() => {
            s.tick();
            true
        }
        // Closed: free the whole window rather than keeping it hidden in memory.
        Some(s) if !s.busy() => {
            *slot = None;
            false
        }
        Some(s) => {
            s.tick();
            false
        }
        None => false,
    };
    let active = settings_open
        || ctx.overlay.visible()
        || ctx.shared.state() != vaani_core::state::State::Idle;
    Some(if active { FAST } else { IDLE })
}
