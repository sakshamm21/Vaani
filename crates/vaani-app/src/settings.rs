//! The settings window: home (status and a place to try dictation), general options,
//! speech models, cloud providers, history and about.
//!
//! Changes are written to the config file straight away; the running app picks them up
//! through the config watcher, so there is no Apply button.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::Sender;
use gtk4::prelude::*;
use gtk4::{Align, Orientation, glib};
use vaani_core::config::{Config, EngineKind, HotkeyMode, Theme};
use vaani_models::ModelInfo;

use crate::controller::{EngineStatus, Input, Shared};
use crate::keys::pretty;

const LANGUAGES: &[(&str, &str)] = &[
    ("auto", "Detect automatically"),
    ("en", "English"),
    ("hi", "Hindi"),
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("nl", "Dutch"),
    ("ru", "Russian"),
    ("uk", "Ukrainian"),
    ("pl", "Polish"),
    ("ja", "Japanese"),
    ("zh", "Chinese"),
    ("ko", "Korean"),
    ("ar", "Arabic"),
    ("bn", "Bengali"),
    ("ta", "Tamil"),
    ("te", "Telugu"),
    ("mr", "Marathi"),
    ("ur", "Urdu"),
];

#[derive(Default)]
struct Download {
    done: u64,
    total: Option<u64>,
    finished: Option<Result<(), String>>,
    cancel: bool,
}

struct ModelRow {
    progress: gtk4::ProgressBar,
}

struct Inner {
    shared: Arc<Shared>,
    ctrl: Sender<Input>,
    window: gtk4::Window,
    stack: gtk4::Stack,
    // Home
    hero_title: gtk4::Label,
    hero_sub: gtk4::Label,
    hero_badge: gtk4::Label,
    hero_action: gtk4::Button,
    hero_progress: gtk4::ProgressBar,
    stat_n: gtk4::Label,
    stat_w: gtk4::Label,
    // Speech
    models_box: gtk4::Box,
    model_rows: RefCell<HashMap<String, ModelRow>>,
    downloads: RefCell<HashMap<String, Arc<Mutex<Download>>>>,
    // General
    key_label: gtk4::Label,
    home_key: gtk4::Label,
    capturing: RefCell<Option<std::sync::mpsc::Receiver<Option<&'static str>>>>,
    capture_button: gtk4::Button,
    theme_drop: gtk4::DropDown,
    theme_btn: gtk4::Button,
    about_logo: gtk4::Image,
    // History
    history_list: gtk4::ListBox,
    history_search: gtk4::SearchEntry,
    last_status: RefCell<Option<EngineStatus>>,
    last_history_check: RefCell<Instant>,
    last_history_top: RefCell<i64>,
}

pub struct Settings {
    inner: Rc<Inner>,
}

fn config_path() -> Option<std::path::PathBuf> {
    vaani_core::paths::config_file()
}

/// Change the config, save it, and update the shared copy so the UI stays consistent.
fn update_config(shared: &Shared, f: impl FnOnce(&mut Config)) {
    let mut c = shared.config.lock().map(|c| c.clone()).unwrap_or_default();
    f(&mut c);
    if let Some(p) = config_path() {
        if let Err(e) = c.save(&p) {
            tracing::warn!("could not save settings: {e}");
            crate::notify::send("Couldn't save settings", &e.to_string());
            return;
        }
    }
    if let Ok(mut s) = shared.config.lock() {
        *s = c;
    }
}

fn config(shared: &Shared) -> Config {
    shared.config.lock().map(|c| c.clone()).unwrap_or_default()
}

// ---------- small layout helpers ----------

fn label(text: &str, class: &str) -> gtk4::Label {
    let l = gtk4::Label::new(Some(text));
    l.set_xalign(0.0);
    l.set_wrap(!matches!(class, "keycap" | "badge" | "stat-num"));
    if !class.is_empty() {
        l.add_css_class(class);
    }
    l
}

fn row(title: &str, subtitle: &str, widget: &impl IsA<gtk4::Widget>) -> gtk4::Box {
    let b = gtk4::Box::new(Orientation::Horizontal, 12);
    b.add_css_class("row");
    let text = gtk4::Box::new(Orientation::Vertical, 2);
    text.set_hexpand(true);
    text.set_valign(Align::Center);
    text.append(&label(title, "row-title"));
    if !subtitle.is_empty() {
        text.append(&label(subtitle, "row-sub"));
    }
    b.append(&text);
    widget.set_valign(Align::Center);
    b.append(widget);
    b
}

fn card(rows: &[gtk4::Box]) -> gtk4::Box {
    let c = gtk4::Box::new(Orientation::Vertical, 0);
    c.add_css_class("card");
    for (i, r) in rows.iter().enumerate() {
        if i > 0 {
            c.append(&gtk4::Separator::new(Orientation::Horizontal));
        }
        c.append(r);
    }
    c
}

fn page(title: &str, sub: &str) -> (gtk4::ScrolledWindow, gtk4::Box) {
    let content = gtk4::Box::new(Orientation::Vertical, 0);
    content.add_css_class("page");
    content.append(&label(title, "page-title"));
    if !sub.is_empty() {
        content.append(&label(sub, "page-sub"));
    }
    let clamp = gtk4::Box::new(Orientation::Vertical, 0);
    clamp.set_halign(Align::Fill);
    clamp.append(&content);
    let sw = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .child(&clamp)
        .vexpand(true)
        .build();
    (sw, content)
}

fn group(parent: &gtk4::Box, title: &str) {
    parent.append(&label(title, "group-title"));
}

fn switch(active: bool, on_change: impl Fn(bool) + 'static) -> gtk4::Switch {
    let s = gtk4::Switch::new();
    s.set_active(active);
    s.connect_active_notify(move |s| on_change(s.is_active()));
    s
}

fn time_ago(at: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(at);
    let d = (now - at).max(0);
    match d {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86_399 => format!("{} h ago", d / 3600),
        _ => format!("{} days ago", d / 86_400),
    }
}

impl Settings {
    pub fn new(shared: Arc<Shared>, ctrl: Sender<Input>) -> Self {
        let window = gtk4::Window::builder()
            .title("Vaani")
            .default_width(900)
            .default_height(640)
            .icon_name(crate::keys::app_icon())
            .build();
        window.add_css_class("vaani-settings");
        let header = gtk4::HeaderBar::new();
        window.set_titlebar(Some(&header));

        let stack = gtk4::Stack::builder()
            .transition_type(gtk4::StackTransitionType::Crossfade)
            .hexpand(true)
            .css_classes(["pages"])
            .build();

        let inner = Rc::new(Inner {
            shared: shared.clone(),
            ctrl,
            window: window.clone(),
            stack: stack.clone(),
            hero_title: label("", "hero-title"),
            hero_sub: label("", "hero-sub"),
            hero_badge: label("", "badge"),
            hero_action: gtk4::Button::new(),
            hero_progress: gtk4::ProgressBar::new(),
            stat_n: label("0", "stat-num"),
            stat_w: label("0", "stat-num"),
            models_box: gtk4::Box::new(Orientation::Vertical, 0),
            model_rows: RefCell::new(HashMap::new()),
            downloads: RefCell::new(HashMap::new()),
            key_label: label("", "keycap"),
            home_key: label("", "keycap"),
            capturing: RefCell::new(None),
            capture_button: gtk4::Button::with_label("Change…"),
            theme_drop: gtk4::DropDown::from_strings(&["Match my desktop", "Light", "Dark"]),
            theme_btn: gtk4::Button::new(),
            about_logo: gtk4::Image::from_icon_name(crate::keys::app_icon()),
            history_list: gtk4::ListBox::new(),
            history_search: gtk4::SearchEntry::new(),
            last_status: RefCell::new(None),
            last_history_check: RefCell::new(Instant::now() - Duration::from_secs(10)),
            last_history_top: RefCell::new(-1),
        });

        stack.add_titled(&build_home(&inner), Some("home"), "Overview");
        stack.add_titled(&build_general(&inner), Some("general"), "General");
        stack.add_titled(&build_speech(&inner), Some("speech"), "Speech Model");
        stack.add_titled(&build_cloud(&inner), Some("cloud"), "Cloud");
        stack.add_titled(&build_history(&inner), Some("history"), "History");
        stack.add_titled(&build_about(&inner), Some("about"), "About");

        let sidebar = gtk4::StackSidebar::new();
        sidebar.set_stack(&stack);
        sidebar.add_css_class("sidebar");
        sidebar.set_width_request(190);

        let body = gtk4::Box::new(Orientation::Horizontal, 0);
        body.append(&sidebar);
        body.append(&gtk4::Separator::new(Orientation::Vertical));
        body.append(&stack);
        window.set_child(Some(&body));

        inner
            .theme_btn
            .set_tooltip_text(Some("Switch light / dark"));
        header.pack_end(&inner.theme_btn);
        {
            let shared = shared.clone();
            let drop = inner.theme_drop.clone();
            inner.theme_btn.connect_clicked(move |_| {
                let next = if crate::style::is_dark() {
                    Theme::Light
                } else {
                    Theme::Dark
                };
                drop.set_selected(theme_index(next));
                update_config(&shared, |c| c.general.theme = next);
            });
        }

        let s = Self { inner };
        s.sync_theme_button();
        s.refresh_home();
        s.rebuild_models();
        s.refresh_history();
        if let Ok(p) = std::env::var("VAANI_PAGE") {
            s.inner.stack.set_visible_child_name(&p);
        }
        window.connect_map(crate::overlay::center_window);
        window.present();
        s
    }

    pub fn is_open(&self) -> bool {
        self.inner.window.is_visible()
    }

    fn sync_theme_button(&self) {
        // Show what you'd switch to.
        let icon = if crate::style::is_dark() {
            "weather-clear-symbolic"
        } else {
            "weather-clear-night-symbolic"
        };
        self.inner.theme_btn.set_icon_name(icon);
        let logo = crate::keys::app_icon();
        if self.inner.window.icon_name().as_deref() != Some(logo) {
            self.inner.window.set_icon_name(Some(logo));
        }
        if self.inner.about_logo.icon_name().as_deref() != Some(logo) {
            self.inner.about_logo.set_icon_name(Some(logo));
        }
    }

    /// Still downloading something, so keep the state around after the window closes.
    pub fn busy(&self) -> bool {
        !self.inner.downloads.borrow().is_empty()
    }

    pub fn present(&self) {
        self.inner.window.present();
    }

    /// Called from the UI timer (~30 Hz).
    pub fn tick(&self) {
        let i = &self.inner;
        self.sync_theme_button();
        // Engine status changes.
        let status = i.shared.engine();
        if i.last_status.borrow().as_ref() != Some(&status) {
            *i.last_status.borrow_mut() = Some(status);
            self.refresh_home();
            self.rebuild_models();
        }
        // Key capture.
        let captured = i
            .capturing
            .borrow()
            .as_ref()
            .and_then(|rx| rx.try_recv().ok());
        if let Some(result) = captured {
            *i.capturing.borrow_mut() = None;
            i.capture_button.set_label("Change…");
            i.capture_button.set_sensitive(true);
            if let Some(name) = result {
                update_config(&i.shared, |c| c.hotkey.key = name.to_string());
            }
            self.refresh_home();
            i.key_label.set_text(&pretty(&config(&i.shared).hotkey.key));
        }
        // Downloads.
        let mut finished = Vec::new();
        for (id, dl) in i.downloads.borrow().iter() {
            let Ok(d) = dl.lock() else { continue };
            let frac = d
                .total
                .map(|t| d.done as f64 / t.max(1) as f64)
                .unwrap_or(0.0);
            let text = format!(
                "{:.0} / {} MB",
                d.done as f64 / 1e6,
                d.total
                    .map(|t| format!("{:.0}", t as f64 / 1e6))
                    .unwrap_or_else(|| "?".into())
            );
            if let Some(r) = i.model_rows.borrow().get(id) {
                r.progress.set_fraction(frac);
                r.progress.set_text(Some(&text));
            }
            if i.hero_progress.is_visible() {
                i.hero_progress.set_fraction(frac);
                i.hero_progress.set_text(Some(&text));
            }
            if let Some(res) = &d.finished {
                finished.push((id.clone(), res.clone()));
            }
        }
        if !finished.is_empty() {
            for (id, res) in finished {
                i.downloads.borrow_mut().remove(&id);
                match res {
                    Ok(()) => {
                        // Use it right away if it's the selected model (first-run flow).
                        let cfg = config(&i.shared);
                        if cfg.engine.local_model == id && cfg.engine.kind == EngineKind::Local {
                            let _ = i.ctrl.send(Input::ReloadEngine);
                        }
                    }
                    Err(e) if e == "download cancelled" => {}
                    Err(e) => crate::notify::send("Model download failed", &e),
                }
            }
            self.refresh_home();
            self.rebuild_models();
        }
        // History: refresh when a new dictation lands.
        if i.last_history_check.borrow().elapsed() > Duration::from_millis(800) {
            *i.last_history_check.borrow_mut() = Instant::now();
            let top = open_history()
                .and_then(|h| h.last().ok().flatten())
                .map(|e| e.id)
                .unwrap_or(0);
            if top != *i.last_history_top.borrow() {
                self.refresh_history();
                self.refresh_home();
            }
        }
    }

    fn refresh_home(&self) {
        let i = &self.inner;
        let cfg = config(&i.shared);
        let key = pretty(&cfg.hotkey.key);
        let downloading = i.downloads.borrow().contains_key(&cfg.engine.local_model);
        i.hero_progress.set_visible(downloading);
        i.hero_action.set_visible(false);
        i.hero_badge.remove_css_class("accent");
        let how = match cfg.hotkey.mode {
            HotkeyMode::PushToTalk => format!(
                "Hold {key} in any app, speak, and let go. Tap {key} for hands-free; tap again to finish. Esc cancels."
            ),
            HotkeyMode::Toggle => {
                format!("Press {key} to start, speak, and press it again to finish. Esc cancels.")
            }
        };
        match i.shared.engine() {
            EngineStatus::Ready(name) => {
                i.hero_title.set_text("Ready");
                i.hero_sub.set_text(&how);
                i.hero_badge.set_text(&name);
            }
            EngineStatus::Loading(name) => {
                i.hero_title.set_text("Loading model");
                i.hero_sub
                    .set_text(&format!("Loading {name}. This takes a few seconds."));
                i.hero_badge.set_text("loading");
                i.hero_badge.add_css_class("accent");
            }
            EngineStatus::Missing(what) if cfg.engine.kind == EngineKind::Local => {
                i.hero_title.set_text(if downloading {
                    "Downloading speech model"
                } else {
                    "Speech model required"
                });
                let size = vaani_models::find(&cfg.engine.local_model)
                    .map(|m| m.download_mb)
                    .unwrap_or(0);
                i.hero_sub.set_text(&format!(
                    "{what} runs entirely on this computer, so your voice never leaves it. It's a one-time {size} MB download."
                ));
                i.hero_badge.set_text("setup");
                i.hero_badge.add_css_class("accent");
                if !downloading {
                    i.hero_action.set_label(&format!("Download {what}"));
                    i.hero_action.set_visible(true);
                }
            }
            EngineStatus::Missing(what) => {
                i.hero_title.set_text("API key required");
                i.hero_sub.set_text(&format!(
                    "The cloud engine needs a {what}. Open the Cloud page to add it."
                ));
                i.hero_badge.set_text("setup");
                i.hero_badge.add_css_class("accent");
            }
            EngineStatus::Failed(e) => {
                i.hero_title.set_text("Speech engine error");
                i.hero_sub.set_text(&e);
                i.hero_badge.set_text("error");
            }
        }
        let (n, words) = open_history()
            .and_then(|h| h.stats().ok())
            .unwrap_or((0, 0));
        i.stat_n.set_text(&n.to_string());
        i.stat_w.set_text(&words.to_string());
        i.key_label.set_text(&key);
        i.home_key.set_text(&key);
    }

    fn rebuild_models(&self) {
        let i = &self.inner;
        while let Some(c) = i.models_box.first_child() {
            i.models_box.remove(&c);
        }
        i.model_rows.borrow_mut().clear();
        let cfg = config(&i.shared);
        let mut rows = Vec::new();
        for m in vaani_models::registry() {
            rows.push(self.model_row(&m, &cfg));
        }
        i.models_box.append(&card(&rows));
    }

    fn model_row(&self, m: &ModelInfo, cfg: &Config) -> gtk4::Box {
        let i = &self.inner;
        let in_use = cfg.engine.kind == EngineKind::Local && cfg.engine.local_model == m.id;
        let installed = m.is_installed();
        let downloading = i.downloads.borrow().contains_key(&m.id);

        let right = gtk4::Box::new(Orientation::Horizontal, 8);
        right.set_valign(Align::Center);
        let progress = gtk4::ProgressBar::new();
        progress.set_show_text(true);
        progress.set_width_request(160);
        progress.set_valign(Align::Center);

        if downloading {
            right.append(&progress);
            let cancel = gtk4::Button::from_icon_name("process-stop-symbolic");
            cancel.set_tooltip_text(Some("Cancel download"));
            let dl = i.downloads.borrow().get(&m.id).cloned();
            cancel.connect_clicked(move |_| {
                if let Some(dl) = &dl {
                    if let Ok(mut d) = dl.lock() {
                        d.cancel = true;
                    }
                }
            });
            right.append(&cancel);
        } else if installed {
            if in_use {
                right.append(&label("In use", "badge"));
            } else {
                let use_btn = gtk4::Button::with_label("Use");
                use_btn.add_css_class("suggested-action");
                let shared = i.shared.clone();
                let id = m.id.clone();
                let weak = Rc::downgrade(&self.inner);
                use_btn.connect_clicked(move |_| {
                    update_config(&shared, |c| {
                        c.engine.kind = EngineKind::Local;
                        c.engine.local_model = id.clone();
                    });
                    if let Some(inner) = weak.upgrade() {
                        Settings { inner }.rebuild_models();
                    }
                });
                right.append(&use_btn);
            }
            if !in_use {
                let rm = gtk4::Button::from_icon_name("user-trash-symbolic");
                rm.set_tooltip_text(Some("Delete this model"));
                let model = m.clone();
                let weak = Rc::downgrade(&self.inner);
                rm.connect_clicked(move |_| {
                    let _ = vaani_models::remove(&model);
                    if let Some(inner) = weak.upgrade() {
                        Settings { inner }.rebuild_models();
                    }
                });
                right.append(&rm);
            }
        } else {
            let dl_btn = gtk4::Button::with_label(&format!("Download ({} MB)", m.download_mb));
            let model = m.clone();
            let weak = Rc::downgrade(&self.inner);
            dl_btn.connect_clicked(move |_| {
                if let Some(inner) = weak.upgrade() {
                    let s = Settings { inner };
                    s.start_download(&model);
                    s.rebuild_models();
                    s.refresh_home();
                }
            });
            right.append(&dl_btn);
        }

        let title = m.name.clone();
        if m.recommended {
            let star = label("Recommended", "badge");
            star.add_css_class("star");
            right.prepend(&star);
        }
        let sub = if m.notes.is_empty() {
            m.languages.clone()
        } else {
            format!("{} · {}", m.languages, m.notes)
        };
        i.model_rows
            .borrow_mut()
            .insert(m.id.clone(), ModelRow { progress });
        row(&title, &sub, &right)
    }

    fn start_download(&self, m: &ModelInfo) {
        let dl = Arc::new(Mutex::new(Download::default()));
        self.inner
            .downloads
            .borrow_mut()
            .insert(m.id.clone(), dl.clone());
        let model = m.clone();
        let _ = std::thread::Builder::new()
            .name("download".into())
            .spawn(move || {
                let mut cb = |done: u64, total: Option<u64>| -> bool {
                    match dl.lock() {
                        Ok(mut d) => {
                            d.done = done;
                            d.total = total;
                            !d.cancel
                        }
                        Err(_) => false,
                    }
                };
                let res = vaani_models::download(&model, &mut cb)
                    .map(|_| ())
                    .map_err(|e| e.to_string());
                if let Ok(mut d) = dl.lock() {
                    d.finished = Some(res);
                }
            });
    }

    fn refresh_history(&self) {
        let i = &self.inner;
        while let Some(c) = i.history_list.first_child() {
            i.history_list.remove(&c);
        }
        let query = i.history_search.text().to_lowercase();
        let entries = open_history()
            .and_then(|h| h.recent(300).ok())
            .unwrap_or_default();
        *i.last_history_top.borrow_mut() = entries.first().map(|e| e.id).unwrap_or(0);
        let mut shown = 0;
        for e in entries {
            if !query.is_empty() && !e.text.to_lowercase().contains(&query) {
                continue;
            }
            shown += 1;
            let b = gtk4::Box::new(Orientation::Horizontal, 10);
            b.add_css_class("row");
            let text = gtk4::Box::new(Orientation::Vertical, 2);
            text.set_hexpand(true);
            let t = label(e.text.trim_end(), "history-text");
            t.set_selectable(true);
            text.append(&t);
            let mut meta = time_ago(e.at);
            if !e.app.is_empty() {
                meta.push_str(&format!(" · {}", e.app));
            }
            if !e.inserted {
                meta.push_str(" · not pasted");
            }
            text.append(&label(&meta, "history-meta"));
            b.append(&text);
            let copy = gtk4::Button::from_icon_name("edit-copy-symbolic");
            copy.set_tooltip_text(Some("Copy"));
            copy.set_valign(Align::Center);
            let s = e.text.trim_end().to_string();
            copy.connect_clicked(move |btn| {
                btn.clipboard().set_text(&s);
            });
            b.append(&copy);
            i.history_list.append(&b);
        }
        if shown == 0 {
            let empty = label(
                if query.is_empty() {
                    "No dictations yet."
                } else {
                    "No matches."
                },
                "row-sub",
            );
            empty.set_margin_top(24);
            empty.set_margin_bottom(24);
            empty.set_xalign(0.5);
            i.history_list.append(&empty);
        }
    }
}

fn open_history() -> Option<vaani_store::History> {
    vaani_core::paths::data_dir()
        .map(|d| d.join("history.db"))
        .and_then(|p| vaani_store::History::open(&p).ok())
}

// ---------- pages ----------

fn build_home(inner: &Rc<Inner>) -> gtk4::ScrolledWindow {
    let (sw, content) = page("Overview", "");
    let hero = gtk4::Box::new(Orientation::Vertical, 8);
    hero.add_css_class("hero");
    let top = gtk4::Box::new(Orientation::Horizontal, 10);
    inner.hero_title.set_hexpand(true);
    top.append(&inner.hero_title);
    inner.hero_badge.set_valign(Align::Center);
    top.append(&inner.hero_badge);
    hero.append(&top);
    hero.append(&inner.hero_sub);
    inner.hero_action.add_css_class("suggested-action");
    inner.hero_action.add_css_class("pill");
    inner.hero_action.set_halign(Align::Start);
    inner.hero_action.set_margin_top(6);
    {
        let weak = Rc::downgrade(inner);
        inner.hero_action.connect_clicked(move |_| {
            let Some(inner) = weak.upgrade() else { return };
            let cfg = config(&inner.shared);
            if let Some(m) = vaani_models::find(&cfg.engine.local_model) {
                let s = Settings { inner };
                s.start_download(&m);
                s.refresh_home();
                s.rebuild_models();
            }
        });
    }
    hero.append(&inner.hero_action);
    inner.hero_progress.set_show_text(true);
    inner.hero_progress.set_margin_top(6);
    inner.hero_progress.set_visible(false);
    hero.append(&inner.hero_progress);
    content.append(&hero);

    group(&content, "Test Area");
    let tryit = gtk4::Box::new(Orientation::Vertical, 6);
    tryit.add_css_class("tryit");
    let hint = label(
        "Click in the box below, then hold your shortcut and speak.",
        "row-sub",
    );
    hint.set_margin_start(14);
    hint.set_margin_top(10);
    tryit.append(&hint);
    let tv = gtk4::TextView::builder()
        .wrap_mode(gtk4::WrapMode::WordChar)
        .left_margin(14)
        .right_margin(14)
        .top_margin(8)
        .bottom_margin(12)
        .height_request(120)
        .build();
    tryit.append(&tv);
    content.append(&tryit);

    group(&content, "Shortcut");
    let keybox = gtk4::Box::new(Orientation::Horizontal, 8);
    keybox.append(&inner.home_key);
    let change = gtk4::Button::with_label("Change");
    {
        let stack = inner.stack.clone();
        change.connect_clicked(move |_| stack.set_visible_child_name("general"));
    }
    keybox.append(&change);
    content.append(&card(&[row(
        "Dictation shortcut",
        "Works in every app: browsers, editors, chat apps and terminals.",
        &keybox,
    )]));
    group(&content, "Usage");
    let tiles = gtk4::Box::new(Orientation::Horizontal, 12);
    tiles.set_homogeneous(true);
    for (num, cls, what) in [
        (&inner.stat_n, "c1", "dictations"),
        (&inner.stat_w, "c2", "words"),
    ] {
        let t = gtk4::Box::new(Orientation::Vertical, 2);
        t.add_css_class("stat");
        num.add_css_class(cls);
        t.append(num);
        t.append(&label(what, "stat-label"));
        tiles.append(&t);
    }
    content.append(&tiles);
    sw
}

fn build_general(inner: &Rc<Inner>) -> gtk4::ScrolledWindow {
    let (sw, content) = page("General", "Shortcut, microphone and how Vaani behaves.");
    let cfg = config(&inner.shared);
    let shared = inner.shared.clone();

    group(&content, "Appearance");
    inner
        .theme_drop
        .set_selected(theme_index(cfg.general.theme));
    {
        let shared = shared.clone();
        inner.theme_drop.connect_selected_notify(move |d| {
            let t = match d.selected() {
                1 => Theme::Light,
                2 => Theme::Dark,
                _ => Theme::System,
            };
            update_config(&shared, |c| c.general.theme = t);
        });
    }
    content.append(&card(&[row(
        "Theme",
        "Light, dark, or follow the system setting.",
        &inner.theme_drop,
    )]));

    group(&content, "Shortcut");
    let keybox = gtk4::Box::new(Orientation::Horizontal, 8);
    inner.key_label.set_text(&pretty(&cfg.hotkey.key));
    keybox.append(&inner.key_label);
    {
        let weak = Rc::downgrade(inner);
        inner.capture_button.connect_clicked(move |b| {
            let Some(inner) = weak.upgrade() else { return };
            b.set_label("Press a key…");
            b.set_sensitive(false);
            let (tx, rx) = std::sync::mpsc::channel();
            *inner.capturing.borrow_mut() = Some(rx);
            std::thread::spawn(move || {
                let r = vaani_hotkey::capture_next_key(Duration::from_secs(6))
                    .ok()
                    .flatten();
                let _ = tx.send(r);
            });
        });
    }
    keybox.append(&inner.capture_button);
    let modes = gtk4::DropDown::from_strings(&[
        "Hold to talk (tap for hands-free)",
        "Press to start, press to stop",
    ]);
    modes.set_selected(match cfg.hotkey.mode {
        HotkeyMode::PushToTalk => 0,
        HotkeyMode::Toggle => 1,
    });
    {
        let shared = shared.clone();
        modes.connect_selected_notify(move |d| {
            let mode = if d.selected() == 1 {
                HotkeyMode::Toggle
            } else {
                HotkeyMode::PushToTalk
            };
            update_config(&shared, |c| c.hotkey.mode = mode);
        });
    }
    content.append(&card(&[
        row(
            "Dictation key",
            "Right Ctrl, Right Alt, a function key or similar. Esc always cancels.",
            &keybox,
        ),
        row("How the key works", "", &modes),
    ]));

    group(&content, "Microphone");
    let mut mics = vec!["System default".to_string()];
    mics.extend(vaani_audio::input_devices());
    let mic_refs: Vec<&str> = mics.iter().map(String::as_str).collect();
    let mic = gtk4::DropDown::from_strings(&mic_refs);
    mic.set_selected(
        mics.iter()
            .position(|m| *m == cfg.audio.device)
            .map(|p| p as u32)
            .unwrap_or(0),
    );
    {
        let shared = shared.clone();
        let mics = mics.clone();
        mic.connect_selected_notify(move |d| {
            let sel = d.selected() as usize;
            let dev = if sel == 0 {
                String::new()
            } else {
                mics.get(sel).cloned().unwrap_or_default()
            };
            update_config(&shared, |c| c.audio.device = dev);
        });
    }
    mic.set_width_request(260);
    content.append(&card(&[row(
        "Input device",
        "If words come out garbled, lower the input volume in Sound settings.",
        &mic,
    )]));

    group(&content, "Feedback");
    let s1 = {
        let shared = shared.clone();
        switch(cfg.general.sounds, move |v| {
            update_config(&shared, |c| c.general.sounds = v)
        })
    };
    let s2 = {
        let shared = shared.clone();
        switch(cfg.general.overlay, move |v| {
            update_config(&shared, |c| c.general.overlay = v)
        })
    };
    content.append(&card(&[
        row(
            "Sounds",
            "Play a short tone when dictation starts and stops.",
            &s1,
        ),
        row(
            "Floating pill",
            "Show a recording indicator at the bottom of the screen.",
            &s2,
        ),
    ]));

    group(&content, "Text");
    let s3 = {
        let shared = shared.clone();
        switch(cfg.cleanup.remove_fillers, move |v| {
            update_config(&shared, |c| c.cleanup.remove_fillers = v)
        })
    };
    let s4 = {
        let shared = shared.clone();
        switch(cfg.insert.trailing_space, move |v| {
            update_config(&shared, |c| c.insert.trailing_space = v)
        })
    };
    content.append(&card(&[
        row(
            "Remove filler words",
            "Remove \"um\", \"uh\" and similar words.",
            &s3,
        ),
        row(
            "Add a space after each dictation",
            "Keeps consecutive dictations separated.",
            &s4,
        ),
    ]));

    group(&content, "Startup and Privacy");
    let s5 = {
        let shared = shared.clone();
        switch(cfg.general.autostart, move |v| {
            update_config(&shared, |c| c.general.autostart = v)
        })
    };
    const KEEP: &[(i64, &str)] = &[
        (0, "Don't keep"),
        (20, "Last 20"),
        (100, "Last 100"),
        (1000, "Last 1,000"),
        (-1, "Keep all"),
    ];
    let keep_names: Vec<&str> = KEEP.iter().map(|(_, n)| *n).collect();
    let keep = gtk4::DropDown::from_strings(&keep_names);
    let current = if cfg.privacy.incognito {
        0
    } else {
        cfg.privacy.keep_history
    };
    keep.set_selected(KEEP.iter().position(|(n, _)| *n == current).unwrap_or(1) as u32);
    {
        let shared = shared.clone();
        keep.connect_selected_notify(move |d| {
            let n = KEEP
                .get(d.selected() as usize)
                .map(|(n, _)| *n)
                .unwrap_or(20);
            update_config(&shared, |c| {
                c.privacy.keep_history = n;
                c.privacy.incognito = false;
            });
            // Apply now, not only on the next dictation.
            if let (Some(h), Ok(n)) = (open_history(), usize::try_from(n)) {
                let _ = h.trim(n);
            }
        });
    }
    content.append(&card(&[
        row("Start Vaani when I log in", "", &s5),
        row(
            "History",
            "How many dictations to keep. Older ones are deleted automatically. Audio is never stored.",
            &keep,
        ),
    ]));
    sw
}

fn build_speech(inner: &Rc<Inner>) -> gtk4::ScrolledWindow {
    let (sw, content) = page(
        "Speech Model",
        "Local models run entirely on this computer and work offline.",
    );
    let cfg = config(&inner.shared);
    let shared = inner.shared.clone();

    group(&content, "Engine");
    let local = gtk4::CheckButton::with_label("On this computer");
    let cloud = gtk4::CheckButton::with_label("Cloud API");
    cloud.set_group(Some(&local));
    local.set_active(cfg.engine.kind == EngineKind::Local);
    cloud.set_active(cfg.engine.kind == EngineKind::Cloud);
    {
        let shared = shared.clone();
        local.connect_toggled(move |b| {
            if b.is_active() {
                update_config(&shared, |c| c.engine.kind = EngineKind::Local);
            }
        });
    }
    {
        let shared = shared.clone();
        let stack = inner.stack.clone();
        cloud.connect_toggled(move |b| {
            if b.is_active() {
                update_config(&shared, |c| {
                    c.engine.kind = EngineKind::Cloud;
                    c.privacy.local_only = false;
                });
                if vaani_store::secrets::get_key(&config(&shared).engine.cloud_provider).is_none() {
                    stack.set_visible_child_name("cloud");
                }
            }
        });
    }
    let radios = gtk4::Box::new(Orientation::Horizontal, 16);
    radios.append(&local);
    radios.append(&cloud);
    let langs: Vec<&str> = LANGUAGES.iter().map(|(_, n)| *n).collect();
    let lang = gtk4::DropDown::from_strings(&langs);
    lang.set_selected(
        LANGUAGES
            .iter()
            .position(|(c, _)| *c == cfg.general.language)
            .unwrap_or(0) as u32,
    );
    {
        let shared = shared.clone();
        lang.connect_selected_notify(move |d| {
            let code = LANGUAGES
                .get(d.selected() as usize)
                .map(|(c, _)| *c)
                .unwrap_or("auto");
            update_config(&shared, |c| c.general.language = code.to_string());
        });
    }
    content.append(&card(&[
        row("Where speech is turned into text", "", &radios),
        row(
            "Language",
            "Used by cloud engines and Whisper. Parakeet detects its 25 languages on its own.",
            &lang,
        ),
    ]));

    group(&content, "Local Models");
    content.append(&inner.models_box);
    sw
}

fn build_cloud(inner: &Rc<Inner>) -> gtk4::ScrolledWindow {
    let (sw, content) = page(
        "Cloud",
        "Optional. Use your own API key for fast, accurate cloud transcription. Audio is sent to the provider only while this engine is selected, and the pill shows a cloud icon.",
    );
    let cfg = config(&inner.shared);
    let shared = inner.shared.clone();
    let providers = vaani_stt::cloud::PROVIDERS;
    let names: Vec<&str> = providers.iter().map(|p| p.name).collect();
    let provider = gtk4::DropDown::from_strings(&names);
    provider.set_selected(
        providers
            .iter()
            .position(|p| p.id == cfg.engine.cloud_provider)
            .unwrap_or(0) as u32,
    );
    let model = gtk4::Entry::builder()
        .text(&cfg.engine.cloud_model)
        .width_chars(26)
        .build();
    let base = gtk4::Entry::builder()
        .text(&cfg.engine.cloud_base_url)
        .placeholder_text("https://…/v1")
        .width_chars(26)
        .build();
    let key = gtk4::PasswordEntry::builder()
        .show_peek_icon(true)
        .width_chars(26)
        .build();
    let key_state = label("", "row-sub");
    let get_link =
        gtk4::LinkButton::with_label("https://console.groq.com/keys", "Get a free Groq key");

    let refresh_key_state = {
        let key_state = key_state.clone();
        let get_link = get_link.clone();
        move |pid: &str| {
            let p = vaani_stt::cloud::provider(pid);
            let has = vaani_store::secrets::get_key(pid).is_some();
            key_state.set_text(if has {
                "A key is saved in your keyring."
            } else {
                "No key saved yet."
            });
            match p {
                Some(p) if !p.key_url.is_empty() => {
                    get_link.set_uri(p.key_url);
                    get_link.set_label(&format!("Get a {} key", p.name));
                    get_link.set_visible(true);
                }
                _ => get_link.set_visible(false),
            }
        }
    };
    refresh_key_state(&cfg.engine.cloud_provider);

    {
        let shared = shared.clone();
        let model = model.clone();
        let refresh = refresh_key_state.clone();
        provider.connect_selected_notify(move |d| {
            if let Some(p) = providers.get(d.selected() as usize) {
                model.set_text(p.default_model);
                update_config(&shared, |c| {
                    c.engine.cloud_provider = p.id.to_string();
                    c.engine.cloud_model = p.default_model.to_string();
                });
                refresh(p.id);
            }
        });
    }
    {
        let shared = shared.clone();
        model.connect_changed(move |e| {
            let v = e.text().to_string();
            update_config(&shared, |c| c.engine.cloud_model = v);
        });
    }
    {
        let shared = shared.clone();
        base.connect_changed(move |e| {
            let v = e.text().trim().to_string();
            update_config(&shared, |c| c.engine.cloud_base_url = v);
        });
    }
    let save = gtk4::Button::with_label("Save key");
    save.add_css_class("suggested-action");
    {
        let shared = shared.clone();
        let key = key.clone();
        let refresh = refresh_key_state.clone();
        let ctrl = inner.ctrl.clone();
        save.connect_clicked(move |_| {
            let pid = config(&shared).engine.cloud_provider;
            let k = key.text().trim().to_string();
            match vaani_store::secrets::set_key(&pid, &k) {
                Ok(()) => {
                    key.set_text("");
                    refresh(&pid);
                    let _ = ctrl.send(Input::ReloadEngine);
                }
                Err(e) => crate::notify::send("Couldn't save the key", &e),
            }
        });
    }
    let keybox = gtk4::Box::new(Orientation::Horizontal, 8);
    keybox.append(&key);
    keybox.append(&save);
    let use_cloud = gtk4::Button::with_label("Use cloud engine");
    {
        let shared = shared.clone();
        use_cloud.connect_clicked(move |_| {
            update_config(&shared, |c| {
                c.engine.kind = EngineKind::Cloud;
                c.privacy.local_only = false;
            });
        });
    }
    let keycol = gtk4::Box::new(Orientation::Vertical, 4);
    keycol.append(&keybox);
    keycol.append(&key_state);
    content.append(&card(&[
        row("Provider", "Groq is fast and has a free tier.", &provider),
        row(
            "API key",
            "Stored in your system keyring, never in a file.",
            &keycol,
        ),
        row("Model", "", &model),
        row(
            "Server address",
            "Only for \"Other (OpenAI-compatible)\", e.g. a local Whisper server.",
            &base,
        ),
    ]));
    let actions = gtk4::Box::new(Orientation::Horizontal, 12);
    actions.set_margin_top(12);
    actions.append(&use_cloud);
    actions.append(&get_link);
    content.append(&actions);
    sw
}

fn build_history(inner: &Rc<Inner>) -> gtk4::ScrolledWindow {
    let (sw, content) = page(
        "History",
        "Every dictation is saved here before it's pasted, so nothing is ever lost.",
    );
    let bar = gtk4::Box::new(Orientation::Horizontal, 8);
    inner.history_search.set_hexpand(true);
    inner
        .history_search
        .set_placeholder_text(Some("Search your dictations"));
    {
        let weak = Rc::downgrade(inner);
        inner.history_search.connect_search_changed(move |_| {
            if let Some(inner) = weak.upgrade() {
                Settings { inner }.refresh_history();
            }
        });
    }
    bar.append(&inner.history_search);
    let clear = gtk4::Button::with_label("Clear all");
    {
        let weak = Rc::downgrade(inner);
        let armed = Rc::new(RefCell::new(false));
        clear.connect_clicked(move |b| {
            if !*armed.borrow() {
                *armed.borrow_mut() = true;
                b.set_label("Click again to delete all");
                b.add_css_class("destructive-action");
                return;
            }
            *armed.borrow_mut() = false;
            b.set_label("Clear all");
            b.remove_css_class("destructive-action");
            if let Some(h) = open_history() {
                let _ = h.clear();
            }
            if let Some(inner) = weak.upgrade() {
                let s = Settings { inner };
                s.refresh_history();
                s.refresh_home();
            }
        });
    }
    bar.append(&clear);
    content.append(&bar);
    inner
        .history_list
        .set_selection_mode(gtk4::SelectionMode::None);
    inner.history_list.add_css_class("card");
    inner.history_list.set_margin_top(12);
    content.append(&inner.history_list);
    sw
}

fn build_about(inner: &Rc<Inner>) -> gtk4::ScrolledWindow {
    let (sw, content) = page("", "");
    if let Some(first) = content.first_child() {
        content.remove(&first);
    }
    let brand = gtk4::Box::new(Orientation::Horizontal, 14);
    inner.about_logo.set_pixel_size(56);
    brand.append(&inner.about_logo);
    let title = label("Vaani", "about-logo");
    title.set_valign(Align::Center);
    brand.append(&title);
    content.append(&brand);
    content.append(&label(
        &format!(
            "Version {}. Open-source voice dictation for Linux. No account, no server, no telemetry.",
            env!("CARGO_PKG_VERSION")
        ),
        "row-sub",
    ));

    let open_folder = |path: Option<std::path::PathBuf>| {
        move |_: &gtk4::Button| {
            if let Some(p) = &path {
                let _ = std::fs::create_dir_all(p);
                let uri = format!("file://{}", p.display());
                let _ = gtk4::gio::AppInfo::launch_default_for_uri(
                    &uri,
                    None::<&gtk4::gio::AppLaunchContext>,
                );
            }
        }
    };
    let logs = gtk4::Button::with_label("Open");
    logs.connect_clicked(open_folder(vaani_core::paths::log_dir()));
    let models = gtk4::Button::with_label("Open");
    models.connect_clicked(open_folder(vaani_models::models_dir()));
    let cfgdir = gtk4::Button::with_label("Open");
    cfgdir.connect_clicked(open_folder(
        vaani_core::paths::config_file().and_then(|p| p.parent().map(|d| d.to_path_buf())),
    ));
    let diag = gtk4::Button::with_label("Copy");
    diag.connect_clicked(|b| {
        let exe = std::env::current_exe().unwrap_or_else(|_| "vaani".into());
        if let Ok(out) = std::process::Command::new(exe).arg("doctor").output() {
            b.clipboard()
                .set_text(&String::from_utf8_lossy(&out.stdout));
            b.set_label("Copied");
        }
    });
    let gh = gtk4::LinkButton::with_label("https://github.com/sakshamm21/Vaani", "GitHub");
    content.append(&gtk4::Box::new(Orientation::Vertical, 8));
    group(&content, "Files and Help");
    content.append(&card(&[
        row(
            "Diagnostics",
            "Copies the `vaani doctor` report, handy for bug reports.",
            &diag,
        ),
        row("Log files", "", &logs),
        row("Downloaded models", "", &models),
        row(
            "Settings file",
            "Plain text; edits apply as soon as you save.",
            &cfgdir,
        ),
        row("Source code", "MIT or Apache-2.0.", &gh),
    ]));
    let _ = glib::MainContext::default();
    sw
}

fn theme_index(t: Theme) -> u32 {
    match t {
        Theme::System => 0,
        Theme::Light => 1,
        Theme::Dark => 2,
    }
}
