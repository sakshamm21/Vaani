//! Text insertion: put the dictated text into whatever app has focus.
//!
//! Linux (see `linux`): clipboard paste with restore on X11 and Wayland, typing as the
//! fallback. Windows (see `windows`): the same approach through the Win32 clipboard and
//! `SendInput`.

use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum InjectError {
    #[error("cannot connect to the X server: {0}")]
    Connect(String),
    #[error("X11 error: {0}")]
    X11(String),
    #[error("cannot use the virtual keyboard (/dev/uinput): {0}")]
    Uinput(String),
    #[error("Wayland clipboard: {0}")]
    Wayland(String),
    #[error("Windows: {0}")]
    Win32(String),
    #[error("the insertion thread stopped")]
    Gone,
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{Injector, uinput_usable, wayland_clipboard};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{Injector, uinput_usable, wayland_clipboard};

/// How text gets into the focused app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Paste, and type the text if the app doesn't take the paste.
    #[default]
    Auto,
    /// Paste only; leave the text on the clipboard if the app doesn't take it.
    Paste,
    /// Type key by key; never touch the clipboard.
    Type,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The focused app read our text after `after`.
    Pasted {
        after: Duration,
        terminal: bool,
        app: String,
    },
    /// Typed key by key (the app ignored the paste, or typing was asked for).
    Typed { app: String, chars: usize },
    /// Nobody read it; the text is left on the clipboard.
    LeftOnClipboard { app: String },
}

const TERMINALS: &[&str] = &[
    "terminal",
    "konsole",
    "kitty",
    "alacritty",
    "xterm",
    "rxvt",
    "tilix",
    "terminator",
    "wezterm",
    "ghostty",
    "foot",
    "st-256color",
    "guake",
    "tilda",
    "yakuake",
    "terminology",
    "sakura",
    "tabby",
    "contour",
    "blackbox",
    "ptyxis",
    "warp",
    "cool-retro-term",
    // Windows (process names)
    "powershell",
    "pwsh",
    "cmd",
    "conhost",
    "openconsole",
    "mintty",
];

/// Whether a window class (lowercase) belongs to a terminal emulator.
pub fn is_terminal(class: &str) -> bool {
    TERMINALS.iter().any(|t| class.contains(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_detection() {
        for t in [
            "gnome-terminal-server",
            "xfce4-terminal",
            "kitty",
            "alacritty",
            "org.wezfurlong.wezterm",
            "xterm",
            "tilix",
            "windowsterminal",
            "powershell",
            "mintty",
        ] {
            assert!(is_terminal(t), "{t}");
        }
        for t in ["firefox", "code", "libreoffice-writer", "xed", "slack"] {
            assert!(!is_terminal(t), "{t}");
        }
    }
}
