//! Linux tray icon (StatusNotifierItem). Cinnamon, KDE, XFCE and MATE show it out of the box.

use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::Sender;
use ksni::TrayMethods;
use vaani_core::state::{Command, State};

use vaani_core::config::Config;

use crate::controller::{Input, Shared};
use crate::tray::{engine_choices, pick, snapshot};

pub struct VaaniTray {
    shared: Arc<Shared>,
    tx: Sender<Input>,
}

impl VaaniTray {
    fn config(&self) -> Config {
        crate::tray::config(&self.shared)
    }

    /// Installed local models, then the cloud engine unless "keep everything local" is on.
    fn engine_menu(&self) -> ksni::MenuItem<Self> {
        use ksni::menu::*;
        let (choices, selected) = engine_choices(&self.config());
        let options = choices
            .iter()
            .map(|(_, label)| RadioItem {
                label: label.clone(),
                ..Default::default()
            })
            .collect();
        let picks: Vec<crate::tray::Choice> = choices.into_iter().map(|(c, _)| c).collect();
        SubMenu {
            label: "Speech engine".into(),
            submenu: vec![
                RadioGroup {
                    selected,
                    select: Box::new(move |t: &mut Self, i| {
                        let Some(choice) = picks.get(i).cloned() else {
                            return;
                        };
                        pick(&t.shared, choice);
                    }),
                    options,
                }
                .into(),
            ],
            ..Default::default()
        }
        .into()
    }

    fn status_line(&self) -> String {
        crate::tray::status_line(&self.shared)
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
            self.engine_menu(),
            CheckmarkItem {
                label: "Pause history".into(),
                checked: self.config().privacy.incognito,
                activate: Box::new(|t: &mut Self| {
                    crate::settings::update_config(&t.shared, |c| {
                        c.privacy.incognito = !c.privacy.incognito
                    });
                }),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: "AI clean-up".into(),
                checked: self.config().cleanup.ai_enabled,
                activate: Box::new(|t: &mut Self| {
                    crate::settings::update_config(&t.shared, |c| {
                        c.cleanup.ai_enabled = !c.cleanup.ai_enabled
                    });
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
    let mut last = None;
    loop {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let now = snapshot(&shared);
        if last.as_ref() != Some(&now) {
            last = Some(now);
            if handle.update(|_| {}).await.is_none() {
                return;
            }
        }
    }
}
