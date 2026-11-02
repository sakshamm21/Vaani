//! "Start Vaani when I log in": an XDG autostart entry in ~/.config/autostart.

use std::path::PathBuf;

const FILE: &str = "io.github.sakshamm21.Vaani.desktop";

fn path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|d| d.join("autostart").join(FILE))
}

/// Create or remove the autostart entry to match `enabled`.
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
