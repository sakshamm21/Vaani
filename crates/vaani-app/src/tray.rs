//! Tray icon (StatusNotifierItem). Cinnamon, KDE, XFCE and MATE show it out of the box.

use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::Sender;
use ksni::TrayMethods;
use vaani_core::state::{Command, State};

use crate::controller::{EngineStatus, Input, Shared};

pub struct VaaniTray {
    shared: Arc<Shared>,
    tx: Sender<Input>,
}

impl VaaniTray {
    fn status_line(&self) -> String {
        let key = self
            .shared
            .config
            .lock()
            .map(|c| c.hotkey.key.clone())
            .unwrap_or_default();
        match (self.shared.state(), self.shared.engine()) {
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
}

impl ksni::Tray for VaaniTray {
    fn id(&self) -> String {
        "vaani".into()
    }
    fn title(&self) -> String {
        "Vaani".into()
    }
    fn icon_name(&self) -> String {
        match self.shared.state() {
            State::Recording => "media-record".into(),
            _ => crate::keys::app_icon().into(),
        }
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Vaani".into(),
            description: self.status_line(),
            ..Default::default()
        }
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        crate::ui::request_settings();
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        let recording = self.shared.state() == State::Recording;
        vec![
            StandardItem {
                label: self.status_line(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: if recording {
                    "Stop dictation".into()
                } else {
                    "Start dictation".into()
                },
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(Input::Cmd(Command::Toggle));
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Paste last dictation".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(Input::PasteLast);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Settings…".into(),
                activate: Box::new(|_| crate::ui::request_settings()),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Quit Vaani".into(),
                activate: Box::new(|_| crate::ui::request_quit()),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Start the tray and refresh it whenever the state or engine status changes.
pub async fn run(shared: Arc<Shared>, tx: Sender<Input>) {
    // At login the panel may start after us: retry for a while.
    let mut attempt = 0;
    let handle = loop {
        let tray = VaaniTray {
            shared: shared.clone(),
            tx: tx.clone(),
        };
        match tray.spawn().await {
            Ok(h) => break h,
            Err(e) if attempt < 24 => {
                if attempt == 0 {
                    tracing::info!("tray not available yet ({e}); retrying");
                }
                attempt += 1;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            Err(e) => {
                tracing::warn!("tray icon unavailable: {e}");
                return;
            }
        }
    };
    let mut last = (State::Idle, EngineStatus::Loading(String::new()));
    loop {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let now = (shared.state(), shared.engine());
        if now != last {
            last = now;
            if handle.update(|_| {}).await.is_none() {
                return;
            }
        }
    }
}
