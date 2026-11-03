//! The tray icon: StatusNotifierItem on Linux (see `tray_sni`; Cinnamon, KDE, XFCE and MATE
//! show it out of the box) and the notification area on Windows (see `tray_win`). Both show
//! the same menu, built from the helpers here.

use std::sync::Arc;

use crossbeam_channel::Sender;
use vaani_core::config::{Config, EngineKind};
use vaani_core::state::State;

use crate::controller::{EngineStatus, Input, Shared};

/// Start the tray icon. Linux: on the tokio runtime this is called from.
pub fn start(shared: Arc<Shared>, tx: Sender<Input>) {
    #[cfg(target_os = "linux")]
    tokio::spawn(crate::tray_sni::run(shared, tx));
    #[cfg(windows)]
    crate::tray_win::start(shared, tx);
}

/// One choice in the tray's speech-engine menu.
#[derive(Clone, PartialEq)]
pub enum Choice {
    Local(String),
    Cloud,
}

fn cloud_label(provider: &str) -> String {
    match provider {
        "groq" => "Groq (cloud)".into(),
        "openai" => "OpenAI (cloud)".into(),
        _ => "Cloud server".into(),
    }
}

pub fn config(shared: &Shared) -> Config {
    shared.config.lock().map(|c| c.clone()).unwrap_or_default()
}

/// Installed local models, then the cloud engine unless "keep everything local" is on;
/// and which one is in use (`usize::MAX` for none).
pub fn engine_choices(cfg: &Config) -> (Vec<(Choice, String)>, usize) {
    let mut choices: Vec<(Choice, String)> = vaani_models::registry()
        .into_iter()
        .filter(|m| m.is_installed())
        .map(|m| (Choice::Local(m.id.clone()), m.name.clone()))
        .collect();
    if !cfg.privacy.local_only {
        choices.push((Choice::Cloud, cloud_label(&cfg.engine.cloud_provider)));
    }
    let selected = choices
        .iter()
        .position(|(c, _)| match c {
            Choice::Local(id) => {
                cfg.engine.kind == EngineKind::Local && *id == cfg.engine.local_model
            }
            Choice::Cloud => cfg.engine.kind == EngineKind::Cloud,
        })
        .unwrap_or(usize::MAX);
    (choices, selected)
}

/// Switch to the picked engine.
pub fn pick(shared: &Arc<Shared>, choice: Choice) {
    crate::settings::update_config(shared, |c| match choice {
        Choice::Local(id) => {
            c.engine.kind = EngineKind::Local;
            c.engine.local_model = id;
        }
        Choice::Cloud => c.engine.kind = EngineKind::Cloud,
    });
}

pub fn status_line(shared: &Shared) -> String {
    let key = shared
        .config
        .lock()
        .map(|c| c.hotkey.key.clone())
        .unwrap_or_default();
    match (shared.state(), shared.engine()) {
        (State::Recording, _) => "Listening…".into(),
        (State::Processing, _) => "Transcribing…".into(),
        (_, EngineStatus::Ready(_)) => {
            format!("Ready: hold {} to dictate", crate::keys::pretty(&key))
        }
        (_, EngineStatus::Loading(m)) => format!("Loading {m}…"),
        (_, EngineStatus::Missing(m)) => format!("{m} not downloaded: open Settings"),
        (_, EngineStatus::Failed(_)) => "Speech engine error: open Settings".into(),
    }
}

/// What the tray shows, compared to decide when to refresh it.
pub fn snapshot(shared: &Shared) -> impl PartialEq + use<> {
    let cfg = shared.config.lock().ok().map(|c| {
        (
            c.privacy.incognito,
            c.cleanup.ai_enabled,
            c.privacy.local_only,
            c.engine.kind,
            c.engine.local_model.clone(),
            c.hotkey.key.clone(),
        )
    });
    (shared.state(), shared.engine(), cfg)
}
