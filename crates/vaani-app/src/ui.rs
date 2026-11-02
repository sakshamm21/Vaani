//! User interface hooks. The GTK overlay, tray and settings window plug in here.

/// Ask the UI to show the settings window.
pub fn request_settings() {
    crate::notify::send(
        "Settings",
        "The settings window is coming in the next update.",
    );
}
