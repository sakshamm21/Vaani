//! Pause music and videos while dictating, and resume exactly those afterwards. On Linux
//! through MPRIS (Spotify, Firefox, Chrome, VLC, Rhythmbox, Celluloid and most other players),
//! on Windows through the system media controls (the same sessions the volume flyout shows:
//! Spotify, browsers, the Media Player app and others).

use std::sync::Mutex;
use std::thread;

/// Players we paused (D-Bus names on Linux, app IDs on Windows), so only those are resumed.
static PAUSED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Pause every player that is playing. Runs in the background so recording starts at once.
pub fn pause() {
    let _ = thread::Builder::new()
        .name("media-pause".into())
        .spawn(|| match pause_playing() {
            Ok(names) if !names.is_empty() => {
                tracing::debug!(?names, "paused media");
                if let Ok(mut p) = PAUSED.lock() {
                    p.extend(names);
                }
            }
            Ok(_) => {}
            Err(e) => tracing::debug!("media pause skipped: {e}"),
        });
}

/// Resume the players paused by [`pause`].
pub fn resume() {
    let names = PAUSED
        .lock()
        .map(|mut p| std::mem::take(&mut *p))
        .unwrap_or_default();
    if names.is_empty() {
        return;
    }
    let _ = thread::Builder::new()
        .name("media-resume".into())
        .spawn(move || {
            if let Err(e) = play(&names) {
                tracing::debug!("media resume failed: {e}");
            }
        });
}

#[cfg(windows)]
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};

#[cfg(windows)]
fn pause_playing() -> windows::core::Result<Vec<String>> {
    let manager = Manager::RequestAsync()?.join()?;
    let mut paused = Vec::new();
    for s in manager.GetSessions()? {
        let playing = s
            .GetPlaybackInfo()
            .and_then(|i| i.PlaybackStatus())
            .is_ok_and(|st| st == Status::Playing);
        if playing && s.TryPauseAsync()?.join().unwrap_or(false) {
            paused.push(s.SourceAppUserModelId()?.to_string());
        }
    }
    Ok(paused)
}

#[cfg(windows)]
fn play(names: &[String]) -> windows::core::Result<()> {
    let manager = Manager::RequestAsync()?.join()?;
    for s in manager.GetSessions()? {
        let id = s.SourceAppUserModelId()?.to_string();
        if names.contains(&id) {
            let _ = s.TryPlayAsync()?.join();
        }
    }
    Ok(())
}

#[cfg(not(windows))]
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
#[cfg(not(windows))]
const PATH: &str = "/org/mpris/MediaPlayer2";

#[cfg(not(windows))]
fn play(names: &[String]) -> zbus::Result<()> {
    let conn = zbus::blocking::Connection::session()?;
    for name in names {
        let r = zbus::blocking::Proxy::new(&conn, name.as_str(), PATH, PLAYER)
            .and_then(|p| p.call_method("Play", &()).map(|_| ()));
        if let Err(e) = r {
            tracing::debug!(%name, "media resume failed: {e}");
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn pause_playing() -> zbus::Result<Vec<String>> {
    let conn = zbus::blocking::Connection::session()?;
    let dbus = zbus::blocking::fdo::DBusProxy::new(&conn)?;
    let mut paused = Vec::new();
    for name in dbus.list_names()? {
        let name = name.to_string();
        if !name.starts_with("org.mpris.MediaPlayer2.") {
            continue;
        }
        let proxy = zbus::blocking::Proxy::new(&conn, name.clone(), PATH, PLAYER)?;
        let playing = proxy
            .get_property::<String>("PlaybackStatus")
            .is_ok_and(|s| s == "Playing");
        if playing && proxy.call_method("Pause", &()).is_ok() {
            paused.push(name);
        }
    }
    Ok(paused)
}
