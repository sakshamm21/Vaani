//! GlobalShortcuts portal backend: the desktop owns the shortcut and tells us when it is
//! pressed and released. It needs no permissions, so it is the fallback when /dev/input
//! can't be read. The desktop asks the user once to confirm the shortcut and lets them
//! change it in its own settings.
//!
//! Limits: desktops don't accept a lone modifier such as Right Ctrl as a shortcut, so a
//! modifier hotkey is offered as Ctrl+Alt+Space instead, and Esc can't be watched without
//! taking it away from every app, so cancelling is done from the pill or the tray.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use crossbeam_channel::Sender;
use futures_util::StreamExt;

use crate::{HotkeyError, HotkeyEvent, Listener, Watch, keysym_for};

const ID: &str = "dictate";
const COMMAND_ID: &str = "command";

/// The trigger we suggest, in the XDG shortcuts format.
fn preferred_trigger(key: &str) -> String {
    match keysym_for(key) {
        // Modifiers can't be shortcuts on their own.
        Some(0xffe1..=0xffee) | None => "CTRL+ALT+space".into(),
        _ => key.to_string(),
    }
}

pub fn listen(watch: Watch, tx: Sender<HotkeyEvent>) -> Result<Listener, HotkeyError> {
    let trigger = preferred_trigger(&watch.key);
    let command = !watch.command.is_empty();
    let key = Arc::new(Mutex::new(watch));
    let stop = Arc::new(AtomicBool::new(false));
    let listener = Listener {
        key,
        stop: stop.clone(),
    };
    let (ready_tx, ready_rx) = crossbeam_channel::bounded(1);
    thread::Builder::new()
        .name("hotkey".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ready_tx.send(Err(HotkeyError::Io(e.to_string())));
                    return;
                }
            };
            rt.block_on(run(trigger, command, tx, stop, ready_tx));
        })
        .map_err(|e| HotkeyError::Io(e.to_string()))?;
    // Binding can wait on the user confirming in a desktop dialog.
    match ready_rx.recv_timeout(Duration::from_secs(120)) {
        Ok(Ok(())) => Ok(listener),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(HotkeyError::Portal(
            "the desktop did not answer the shortcut request".into(),
        )),
    }
}

async fn run(
    trigger: String,
    command: bool,
    tx: Sender<HotkeyEvent>,
    stop: Arc<AtomicBool>,
    ready: Sender<Result<(), HotkeyError>>,
) {
    let p = |e: ashpd::Error| HotkeyError::Portal(e.to_string());
    let setup = async {
        let portal = GlobalShortcuts::new().await.map_err(p)?;
        let session = portal.create_session(Default::default()).await.map_err(p)?;
        let shortcut = NewShortcut::new(ID, "Dictate (hold to talk)")
            .preferred_trigger(Some(trigger.as_str()));
        let mut shortcuts = vec![shortcut];
        if command {
            // No suggested keys: the desktop asks the user to pick them.
            shortcuts.push(NewShortcut::new(
                COMMAND_ID,
                "Command mode: change the selected text (hold to talk)",
            ));
        }
        let bound = portal
            .bind_shortcuts(&session, &shortcuts, None, Default::default())
            .await
            .map_err(p)?
            .response()
            .map_err(p)?;
        for s in bound.shortcuts() {
            tracing::info!(
                id = s.id(),
                trigger = s.trigger_description(),
                "hotkey set (portal)"
            );
        }
        let down = portal.receive_activated().await.map_err(p)?;
        let up = portal.receive_deactivated().await.map_err(p)?;
        Ok::<_, HotkeyError>((portal, session, down, up))
    };
    let (_portal, _session, mut down, mut up) = match setup.await {
        Ok(v) => {
            let _ = ready.send(Ok(()));
            v
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let mut held = false;
    let mut cmd_held = false;
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    while !stop.load(Ordering::Relaxed) {
        tokio::select! {
            Some(a) = down.next() => {
                if a.shortcut_id() == ID && !held {
                    held = true;
                    let _ = tx.send(HotkeyEvent::Down);
                } else if a.shortcut_id() == COMMAND_ID && !cmd_held {
                    cmd_held = true;
                    let _ = tx.send(HotkeyEvent::CommandDown);
                }
            }
            Some(d) = up.next() => {
                if d.shortcut_id() == ID && held {
                    held = false;
                    let _ = tx.send(HotkeyEvent::Up);
                } else if d.shortcut_id() == COMMAND_ID && cmd_held {
                    cmd_held = false;
                    let _ = tx.send(HotkeyEvent::CommandUp);
                }
            }
            _ = tick.tick() => {}
        }
    }
}

/// Whether the desktop offers the GlobalShortcuts portal.
pub fn available() -> bool {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return false;
    };
    rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), GlobalShortcuts::new())
            .await
            .is_ok_and(|r| r.is_ok())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_become_a_combo() {
        assert_eq!(preferred_trigger("RightCtrl"), "CTRL+ALT+space");
        assert_eq!(preferred_trigger("Super"), "CTRL+ALT+space");
        assert_eq!(preferred_trigger("F9"), "F9");
        assert_eq!(preferred_trigger("Insert"), "Insert");
    }
}
