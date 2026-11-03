//! Windows tray icon, in the notification area. Left click opens Settings; right click shows
//! the same menu as on Linux.
//!
//! The icon and its menu belong to the thread that made them, which must keep dispatching
//! window messages, so the tray runs on a thread of its own and rebuilds the menu whenever
//! the state it shows changes.

#![allow(unsafe_code)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use tray_icon::menu::{
    CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use vaani_core::state::{Command, State};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW, QS_ALLINPUT,
    TranslateMessage,
};

use crate::controller::{Input, Shared};
use crate::tray::{Choice, config, engine_choices, pick, snapshot, status_line};

/// Icon resource ordinals in vaani.exe (see build.rs).
const ICON_APP: u16 = 1;
const ICON_RECORDING: u16 = 2;

pub fn start(shared: Arc<Shared>, tx: Sender<Input>) {
    let _ = std::thread::Builder::new()
        .name("tray".into())
        .spawn(move || run(shared, tx));
}

/// What each menu item does, by its id.
enum Action {
    Toggle,
    PasteLast,
    Engine(Choice),
    Incognito,
    AiCleanup,
    Settings,
    Quit,
}

fn build_menu(shared: &Shared) -> (Menu, Vec<(MenuId, Action)>) {
    let cfg = config(shared);
    let recording = shared.state() == State::Recording;
    let mut actions = Vec::new();
    let menu = Menu::new();

    let status = MenuItem::new(status_line(shared), false, None);
    let toggle = MenuItem::new(
        if recording {
            "Stop dictation"
        } else {
            "Start dictation"
        },
        true,
        None,
    );
    actions.push((toggle.id().clone(), Action::Toggle));
    let paste = MenuItem::new("Paste last dictation", true, None);
    actions.push((paste.id().clone(), Action::PasteLast));

    let engines = Submenu::new("Speech engine", true);
    let (choices, selected) = engine_choices(&cfg);
    for (i, (choice, label)) in choices.into_iter().enumerate() {
        let item = CheckMenuItem::new(label, true, i == selected, None);
        actions.push((item.id().clone(), Action::Engine(choice)));
        let _ = engines.append(&item);
    }
    let incognito = CheckMenuItem::new("Pause history", true, cfg.privacy.incognito, None);
    actions.push((incognito.id().clone(), Action::Incognito));
    let ai = CheckMenuItem::new("AI clean-up", true, cfg.cleanup.ai_enabled, None);
    actions.push((ai.id().clone(), Action::AiCleanup));
    let settings = MenuItem::new("Settings…", true, None);
    actions.push((settings.id().clone(), Action::Settings));
    let quit = MenuItem::new("Quit Vaani", true, None);
    actions.push((quit.id().clone(), Action::Quit));

    let _ = menu.append_items(&[
        &status,
        &PredefinedMenuItem::separator(),
        &toggle,
        &paste,
        &PredefinedMenuItem::separator(),
        &engines,
        &incognito,
        &ai,
        &PredefinedMenuItem::separator(),
        &settings,
        &quit,
    ]);
    (menu, actions)
}

fn icon(recording: bool) -> Option<Icon> {
    Icon::from_resource(if recording { ICON_RECORDING } else { ICON_APP }, None).ok()
}

/// Windows cuts tooltips at 127 characters.
fn tooltip(shared: &Shared) -> String {
    let t = format!("Vaani: {}", status_line(shared));
    t.chars().take(120).collect()
}

fn make(shared: &Shared) -> Option<(TrayIcon, Vec<(MenuId, Action)>)> {
    let (menu, actions) = build_menu(shared);
    let mut b = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip(tooltip(shared));
    if let Some(i) = icon(false) {
        b = b.with_icon(i);
    }
    match b.build() {
        Ok(t) => Some((t, actions)),
        Err(e) => {
            tracing::warn!("tray icon unavailable: {e}");
            None
        }
    }
}

fn run(shared: Arc<Shared>, tx: Sender<Input>) {
    // At sign-in Explorer may start after us: retry for a while.
    let mut made = None;
    for attempt in 0..24 {
        made = make(&shared);
        if made.is_some() {
            break;
        }
        if attempt == 0 {
            tracing::info!("tray not available yet; retrying");
        }
        pump_for(Duration::from_secs(5));
    }
    let Some((tray, mut actions)) = made else {
        return;
    };
    tracing::info!("tray icon shown");
    let mut last = snapshot(&shared);
    let mut last_check = Instant::now();
    loop {
        pump_for(Duration::from_millis(50));
        while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = ev
            {
                crate::ui::request_settings();
            }
        }
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            let Some(i) = actions.iter().position(|(id, _)| *id == ev.id) else {
                continue;
            };
            match &actions[i].1 {
                Action::Toggle => {
                    let _ = tx.send(Input::Cmd(Command::Toggle));
                }
                Action::PasteLast => {
                    let _ = tx.send(Input::PasteLast);
                }
                Action::Engine(c) => pick(&shared, c.clone()),
                Action::Incognito => crate::settings::update_config(&shared, |c| {
                    c.privacy.incognito = !c.privacy.incognito
                }),
                Action::AiCleanup => crate::settings::update_config(&shared, |c| {
                    c.cleanup.ai_enabled = !c.cleanup.ai_enabled
                }),
                Action::Settings => crate::ui::request_settings(),
                Action::Quit => crate::ui::request_quit(),
            }
        }
        if last_check.elapsed() >= Duration::from_millis(300) {
            last_check = Instant::now();
            let now = snapshot(&shared);
            if now != last {
                last = now;
                let (menu, a) = build_menu(&shared);
                tray.set_menu(Some(Box::new(menu)));
                actions = a;
                let _ = tray.set_tooltip(Some(tooltip(&shared)));
                let _ = tray.set_icon(icon(shared.state() == State::Recording));
            }
        }
    }
}

/// Dispatch this thread's window messages, waiting at most `dur` for one to arrive.
fn pump_for(dur: Duration) {
    let end = Instant::now() + dur;
    loop {
        let mut msg = MSG::default();
        // SAFETY: standard message dispatch for this thread's (tray) windows.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        // SAFETY: waiting for input on this thread's queue, with a timeout.
        unsafe {
            MsgWaitForMultipleObjects(None, false, left.as_millis() as u32, QS_ALLINPUT);
        }
    }
}
