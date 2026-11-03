//! "Start Vaani when I log in": an XDG autostart entry in ~/.config/autostart on Linux, the
//! per-user Run key in the registry on Windows.

#[cfg(windows)]
const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Create or remove the Run entry to match `enabled`.
#[cfg(windows)]
pub fn sync(enabled: bool) {
    let Ok(key) = windows_registry::CURRENT_USER.create(RUN) else {
        return;
    };
    if enabled {
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let value = format!("\"{}\" run", exe.display());
        if key.get_string("Vaani").ok().as_deref() != Some(value.as_str()) {
            if let Err(e) = key.set_string("Vaani", &value) {
                tracing::warn!("could not set Vaani to start at sign-in: {e}");
            }
        }
    } else if key.get_string("Vaani").is_ok() {
        let _ = key.remove_value("Vaani");
    }
}

#[cfg(not(windows))]
use std::path::PathBuf;

#[cfg(not(windows))]
const FILE: &str = "io.github.sakshamm21.Vaani.desktop";

#[cfg(not(windows))]
fn path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|d| d.join("autostart").join(FILE))
}

/// Create or remove the autostart entry to match `enabled`.
#[cfg(not(windows))]
pub fn sync(enabled: bool) {
    let Some(p) = path() else { return };
    if enabled {
        let exe = std::env::current_exe()
            .ok()
            .map(|e| e.display().to_string())
            .unwrap_or_else(|| "vaani".into());
        let body = format!(
            "[Desktop Entry]\nType=Application\nName=Vaani\nComment=Voice dictation\nExec={exe} run\nIcon={icon}\nTerminal=false\nX-GNOME-Autostart-enabled=true\nX-GNOME-Autostart-Delay=3\n",
            icon = crate::keys::APP_ICON
        );
        if std::fs::read_to_string(&p).ok().as_deref() != Some(body.as_str()) {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            if let Err(e) = std::fs::write(&p, body) {
                tracing::warn!("could not write autostart entry: {e}");
            }
        }
    } else if p.exists() {
        let _ = std::fs::remove_file(&p);
    }
}
