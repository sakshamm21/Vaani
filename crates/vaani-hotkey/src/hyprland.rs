//! Hyprland: its GlobalShortcuts portal registers Vaani's shortcuts but leaves them without
//! keys until the user binds them in hyprland.conf. So Vaani binds them itself through
//! Hyprland's control socket, with the key chosen in Settings (lone modifiers included),
//! and binds them again whenever Hyprland reloads its config, which drops runtime binds.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::Watch;

/// The directory holding this Hyprland instance's sockets, when running under Hyprland.
fn instance_dir() -> Option<PathBuf> {
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    [
        PathBuf::from(runtime).join("hypr").join(&sig),
        PathBuf::from("/tmp/hypr").join(&sig),
    ]
    .into_iter()
    .find(|d| d.join(".socket.sock").exists())
}

pub fn running() -> bool {
    instance_dir().is_some()
}

fn request(cmd: &str) -> Option<String> {
    let mut s = UnixStream::connect(instance_dir()?.join(".socket.sock")).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    s.write_all(cmd.as_bytes()).ok()?;
    let mut out = String::new();
    s.read_to_string(&mut out).ok()?;
    Some(out)
}

/// Hyprland's name for a key, with the modifier it sets when it is itself a modifier
/// (pressing Right Ctrl alone is seen as CTRL + Control_R).
fn hypr_key(name: &str) -> Option<(&'static str, &'static str)> {
    Some(match name.to_ascii_lowercase().as_str() {
        "rightctrl" => ("CTRL", "Control_R"),
        "leftctrl" => ("CTRL", "Control_L"),
        "rightalt" => ("ALT", "Alt_R"),
        "leftalt" => ("ALT", "Alt_L"),
        "rightshift" => ("SHIFT", "Shift_R"),
        "rightsuper" => ("SUPER", "Super_R"),
        "super" => ("SUPER", "Super_L"),
        "menu" => ("", "Menu"),
        "insert" => ("", "Insert"),
        "pause" => ("", "Pause"),
        "scrolllock" => ("", "Scroll_Lock"),
        f if f.starts_with('f') => {
            const F: [&str; 12] = [
                "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
            ];
            let n: usize = f[1..].parse().ok()?;
            ("", F.get(n.checked_sub(1)?)?)
        }
        _ => return None,
    })
}

/// Bind (or with an empty key, unbind) the shortcut `id` of Vaani's portal session.
fn bind(id: &str, key: &str, old: &str) {
    let target = format!("{}:{id}", super::portal::APP_ID);
    // Hyprland 0.57+ is configured in Lua. Keep the bind object so only Vaani's own bind is
    // removed when the key changes.
    let mut lua = format!(
        "vaani_binds = vaani_binds or {{}}; \
         if vaani_binds['{id}'] then vaani_binds['{id}']:unbind(); vaani_binds['{id}'] = nil end;"
    );
    if let Some((m, k)) = hypr_key(key) {
        let keys = if m.is_empty() {
            k.to_string()
        } else {
            format!("{m} + {k}")
        };
        lua.push_str(&format!(
            " vaani_binds['{id}'] = hl.bind('{keys}', hl.dsp.global('{target}'))"
        ));
    }
    match request(&format!("eval {lua}")) {
        Some(r) if r.trim() == "ok" => {}
        _ => {
            // Older Hyprland (hyprlang config).
            if let Some((m, k)) = hypr_key(old) {
                let _ = request(&format!("keyword unbind {m},{k}"));
            }
            if let Some((m, k)) = hypr_key(key) {
                let r = request(&format!("keyword bind {m},{k},global,{target}"));
                if r.as_deref().map(str::trim) != Some("ok") {
                    tracing::warn!(?r, "Hyprland did not take the shortcut");
                    return;
                }
            }
        }
    }
    if !key.is_empty() {
        tracing::info!(id, key, "shortcut bound in Hyprland");
    }
}

/// Keep Hyprland's binds for `watch` in place until `stop` is set.
pub fn keep_bound(
    watch: Arc<Mutex<Watch>>,
    stop: Arc<AtomicBool>,
    dictate: &'static str,
    command: &'static str,
) {
    let current = || watch.lock().map(|w| w.clone()).unwrap_or_default();
    let apply = |now: &Watch, was: &Watch| {
        if now.key != was.key {
            bind(dictate, &now.key, &was.key);
        }
        if now.command != was.command {
            bind(command, &now.command, &was.command);
        }
    };
    let mut bound = current();
    apply(&bound, &Watch::default());

    // Config reloads drop runtime binds: listen for them on the event socket.
    let reloaded = Arc::new(AtomicBool::new(false));
    if let Some(path) = instance_dir().map(|d| d.join(".socket2.sock")) {
        let reloaded = reloaded.clone();
        let stop = stop.clone();
        let _ = thread::Builder::new()
            .name("hyprland-events".into())
            .spawn(move || {
                let Ok(s) = UnixStream::connect(path) else {
                    return;
                };
                for line in BufReader::new(s).lines() {
                    let Ok(line) = line else { break };
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if line.starts_with("configreloaded>>") {
                        reloaded.store(true, Ordering::Relaxed);
                    }
                }
            });
    }
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(500));
        let now = current();
        if reloaded.swap(false, Ordering::Relaxed) {
            apply(&now, &Watch::default());
        } else {
            apply(&now, &bound);
        }
        bound = now;
    }
    // Leave the user's Hyprland as it was.
    apply(&Watch::default(), &bound);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names() {
        assert_eq!(hypr_key("RightCtrl"), Some(("CTRL", "Control_R")));
        assert_eq!(hypr_key("F9"), Some(("", "F9")));
        assert_eq!(hypr_key("F12"), Some(("", "F12")));
        assert_eq!(hypr_key("F13"), None);
        assert_eq!(hypr_key("F0"), None);
        assert_eq!(hypr_key(""), None);
    }
}
