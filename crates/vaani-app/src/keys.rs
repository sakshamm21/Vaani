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

/// "RightCtrl" -> "Right Ctrl".
pub fn pretty(key: &str) -> String {
    let mut out = String::new();
    for (i, c) in key.char_indices() {
        if i > 0 && c.is_ascii_uppercase() && !key[..i].ends_with('F') {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn pretty_names() {
        assert_eq!(super::pretty("RightCtrl"), "Right Ctrl");
        assert_eq!(super::pretty("F9"), "F9");
        assert_eq!(super::pretty("ScrollLock"), "Scroll Lock");
    }
}
