//! Names shown to people.

pub const APP_ICON: &str = "io.github.sakshamm21.Vaani";
/// Same logo with dark bars, used while Vaani is in its dark theme.
pub const APP_ICON_DARK: &str = "io.github.sakshamm21.Vaani-dark";

/// The logo variant that matches Vaani's current theme.
pub fn app_icon() -> &'static str {
    if crate::style::is_dark() {
        APP_ICON_DARK
    } else {
        APP_ICON
    }
}

/// The dictation shortcut to show: the desktop's own when it manages it (Wayland portal),
/// otherwise the configured key.
pub fn shortcut(key: &str) -> String {
    vaani_hotkey::portal_trigger().unwrap_or_else(|| pretty(key))
}

/// "RightCtrl" -> "Right Ctrl", "Ctrl+Super" -> "Ctrl + Win" (Windows) or "Ctrl + Super".
pub fn pretty(key: &str) -> String {
    key.split('+')
        .map(|part| {
            let part = part.trim();
            let part = if cfg!(windows) {
                part.replace("Super", "Win")
            } else {
                part.to_string()
            };
            let mut out = String::new();
            for (i, c) in part.char_indices() {
                if i > 0 && c.is_ascii_uppercase() && !part[..i].ends_with('F') {
                    out.push(' ');
                }
                out.push(c);
            }
            out
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

#[cfg(test)]
mod tests {
    #[test]
    fn pretty_names() {
        assert_eq!(super::pretty("RightCtrl"), "Right Ctrl");
        assert_eq!(super::pretty("F9"), "F9");
        assert_eq!(super::pretty("ScrollLock"), "Scroll Lock");
        let win = if cfg!(windows) { "Win" } else { "Super" };
        assert_eq!(super::pretty("Ctrl+Super"), format!("Ctrl + {win}"));
        assert_eq!(super::pretty("Alt+F9"), "Alt + F9");
    }
}
