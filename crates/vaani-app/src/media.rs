//! Pause music and videos while dictating, through MPRIS (Spotify, Firefox, Chrome, VLC,
//! Rhythmbox, Celluloid and most other Linux players), and resume exactly those afterwards.

use std::sync::Mutex;
use std::thread;

/// Players we paused, by D-Bus name, so only those are resumed.
static PAUSED: Mutex<Vec<String>> = Mutex::new(Vec::new());

const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const PATH: &str = "/org/mpris/MediaPlayer2";

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
            let Ok(conn) = zbus::blocking::Connection::session() else {
                return;
            };
            for name in names {
                let r = zbus::blocking::Proxy::new(&conn, name.as_str(), PATH, PLAYER)
                    .and_then(|p| p.call_method("Play", &()).map(|_| ()));
                if let Err(e) = r {
                    tracing::debug!(%name, "media resume failed: {e}");
                }
            }
        });
}

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
