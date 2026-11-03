//! Work out the display server, desktop and available input paths.

use std::collections::HashMap;
#[cfg(not(windows))]
use std::env;
#[cfg(not(windows))]
use std::path::Path;
use std::path::PathBuf;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionType {
    X11,
    Wayland,
    /// A text console or SSH session with no display.
    Tty,
    /// A Windows desktop session.
    Windows,
    Unknown,
}

/// The desktop family. Each family needs different hotkey, insertion and overlay backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Desktop {
    Cinnamon,
    Gnome,
    Kde,
    Xfce,
    Mate,
    Budgie,
    Cosmic,
    /// Sway, Hyprland, river, Wayfire, labwc and other wlroots-style compositors.
    Wlroots,
    /// The Windows shell (Explorer).
    Windows,
    Other,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub session_type: SessionType,
    pub desktop: Desktop,
    /// Raw `XDG_CURRENT_DESKTOP`, for bug reports.
    pub desktop_raw: Option<String>,
    pub x11_display: Option<String>,
    pub wayland_display: Option<String>,
    /// Running under XWayland (both DISPLAY and WAYLAND_DISPLAY set).
    pub xwayland: bool,
    pub dbus_session_bus: bool,
    /// We can open `/dev/uinput` for writing (needed for the uinput fallbacks).
    pub uinput_writable: bool,
    /// We can read at least one `/dev/input/event*` device (needed for the evdev hotkey backend).
    pub evdev_readable: bool,
    /// Helper programs found on PATH, by name.
    pub tools: Vec<(String, bool)>,
}

/// Programs that later backends or fallbacks may use. Reported, never required.
#[cfg(not(windows))]
const TOOLS: &[&str] = &["ibus", "wl-copy", "xdotool", "wtype", "ydotool"];

/// Probe the real environment.
#[cfg(windows)]
pub fn probe() -> SessionInfo {
    SessionInfo {
        session_type: SessionType::Windows,
        desktop: Desktop::Windows,
        desktop_raw: None,
        x11_display: None,
        wayland_display: None,
        xwayland: false,
        dbus_session_bus: false,
        uinput_writable: false,
        evdev_readable: false,
        tools: Vec::new(),
    }
}

/// Probe the real environment.
#[cfg(not(windows))]
pub fn probe() -> SessionInfo {
    let vars: HashMap<String, String> = env::vars().collect();
    let path = vars.get("PATH").cloned().unwrap_or_default();
    let mut info = from_env(&vars);
    info.uinput_writable = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/uinput")
        .is_ok();
    info.evdev_readable = any_evdev_readable(Path::new("/dev/input"));
    info.tools = TOOLS
        .iter()
        .map(|t| ((*t).to_string(), on_path(t, &path)))
        .collect();
    info
}

/// The part of the probe that depends only on environment variables (unit-testable).
pub fn from_env(vars: &HashMap<String, String>) -> SessionInfo {
    let get = |k: &str| vars.get(k).filter(|v| !v.is_empty()).cloned();
    let x11_display = get("DISPLAY");
    let wayland_display = get("WAYLAND_DISPLAY");
    let xdg_type = get("XDG_SESSION_TYPE").map(|s| s.to_ascii_lowercase());

    let session_type = match xdg_type.as_deref() {
        Some("wayland") => SessionType::Wayland,
        Some("x11") => SessionType::X11,
        Some("tty") if x11_display.is_none() && wayland_display.is_none() => SessionType::Tty,
        _ if wayland_display.is_some() => SessionType::Wayland,
        _ if x11_display.is_some() => SessionType::X11,
        _ => SessionType::Unknown,
    };

    let desktop_raw = get("XDG_CURRENT_DESKTOP");
    let desktop = desktop_raw
        .as_deref()
        .map(classify_desktop)
        .or_else(|| {
            // Compositors that don't set XDG_CURRENT_DESKTOP but set their own variable.
            if get("HYPRLAND_INSTANCE_SIGNATURE").is_some() || get("SWAYSOCK").is_some() {
                Some(Desktop::Wlroots)
            } else {
                None
            }
        })
        .unwrap_or(Desktop::Other);

    SessionInfo {
        session_type,
        desktop,
        desktop_raw,
        xwayland: session_type == SessionType::Wayland && x11_display.is_some(),
        x11_display,
        wayland_display,
        dbus_session_bus: get("DBUS_SESSION_BUS_ADDRESS").is_some()
            || get("XDG_RUNTIME_DIR")
                .map(|d| PathBuf::from(d).join("bus").exists())
                .unwrap_or(false),
        uinput_writable: false,
        evdev_readable: false,
        tools: Vec::new(),
    }
}

/// `XDG_CURRENT_DESKTOP` is a colon-separated list, e.g. "X-Cinnamon" or "ubuntu:GNOME".
fn classify_desktop(raw: &str) -> Desktop {
    for part in raw.split(':') {
        let p = part.to_ascii_lowercase();
        let d = match p.trim_start_matches("x-") {
            "cinnamon" => Desktop::Cinnamon,
            "gnome" | "gnome-classic" | "gnome-flashback" | "unity" | "pantheon" => Desktop::Gnome,
            "kde" | "plasma" => Desktop::Kde,
            "xfce" => Desktop::Xfce,
            "mate" => Desktop::Mate,
            "budgie" | "budgie-desktop" => Desktop::Budgie,
            "cosmic" => Desktop::Cosmic,
            "sway" | "hyprland" | "river" | "wayfire" | "labwc" | "niri" | "wlroots" => {
                Desktop::Wlroots
            }
            _ => continue,
        };
        return d;
    }
    Desktop::Other
}

#[cfg(not(windows))]
fn any_evdev_readable(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        e.file_name().to_string_lossy().starts_with("event")
            && std::fs::File::open(e.path()).is_ok()
    })
}

#[cfg(not(windows))]
fn on_path(name: &str, path: &str) -> bool {
    path.split(':')
        .filter(|d| !d.is_empty())
        .any(|d| Path::new(d).join(name).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn mint_cinnamon_x11() {
        let i = from_env(&env(&[
            ("XDG_SESSION_TYPE", "x11"),
            ("DISPLAY", ":0"),
            ("XDG_CURRENT_DESKTOP", "X-Cinnamon"),
        ]));
        assert_eq!(i.session_type, SessionType::X11);
        assert_eq!(i.desktop, Desktop::Cinnamon);
        assert!(!i.xwayland);
    }

    #[test]
    fn ubuntu_gnome_wayland_with_xwayland() {
        let i = from_env(&env(&[
            ("XDG_SESSION_TYPE", "wayland"),
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("DISPLAY", ":0"),
            ("XDG_CURRENT_DESKTOP", "ubuntu:GNOME"),
        ]));
        assert_eq!(i.session_type, SessionType::Wayland);
        assert_eq!(i.desktop, Desktop::Gnome);
        assert!(i.xwayland);
    }

    #[test]
    fn kde_and_hyprland() {
        let kde = from_env(&env(&[
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("XDG_CURRENT_DESKTOP", "KDE"),
        ]));
        assert_eq!(kde.session_type, SessionType::Wayland);
        assert_eq!(kde.desktop, Desktop::Kde);

        let hypr = from_env(&env(&[
            ("WAYLAND_DISPLAY", "wayland-1"),
            ("HYPRLAND_INSTANCE_SIGNATURE", "abc"),
        ]));
        assert_eq!(hypr.desktop, Desktop::Wlroots);
    }

    #[test]
    fn no_display_is_tty_or_unknown() {
        assert_eq!(
            from_env(&env(&[("XDG_SESSION_TYPE", "tty")])).session_type,
            SessionType::Tty
        );
        assert_eq!(from_env(&env(&[])).session_type, SessionType::Unknown);
    }

    #[test]
    fn empty_vars_count_as_unset() {
        let i = from_env(&env(&[("DISPLAY", ""), ("WAYLAND_DISPLAY", "")]));
        assert_eq!(i.session_type, SessionType::Unknown);
    }
}
