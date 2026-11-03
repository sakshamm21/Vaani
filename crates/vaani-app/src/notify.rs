//! Desktop notifications: org.freedesktop.Notifications on Linux (works on every desktop),
//! toast notifications on Windows.

/// The AppUserModelID Windows files Vaani's notifications under (also set on the Start menu
/// shortcut by the installer).
#[cfg(windows)]
pub const APP_ID: &str = "io.github.sakshamm21.Vaani";

/// Register the app ID with a name and icon, so toasts show "Vaani" even when it runs from
/// the portable zip (no Start menu shortcut).
#[cfg(windows)]
pub fn register() {
    let icon = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent()?.parent().map(|d| d.join("vaani.ico")))
        .filter(|p| p.is_file());
    let r = (|| -> windows_registry::Result<()> {
        let k = windows_registry::CURRENT_USER
            .create(format!(r"Software\Classes\AppUserModelId\{APP_ID}"))?;
        k.set_string("DisplayName", "Vaani")?;
        if let Some(icon) = &icon {
            k.set_string("IconUri", icon.display().to_string())?;
        }
        Ok(())
    })();
    if let Err(e) = r {
        tracing::debug!("could not register the notification app ID: {e}");
    }
}

#[cfg(windows)]
pub fn send(summary: &str, body: &str) {
    let summary = summary.to_string();
    let body = body.to_string();
    let _ = std::thread::Builder::new()
        .name("notify".into())
        .spawn(move || {
            let r = tauri_winrt_notification::Toast::new(APP_ID)
                .title(&summary)
                .text1(&body)
                .show();
            if let Err(e) = r {
                tracing::debug!("notification failed: {e}");
            }
        });
}

#[cfg(not(windows))]
pub fn send(summary: &str, body: &str) {
    use std::collections::HashMap;
    use zbus::zvariant::Value;

    let summary = summary.to_string();
    let body = body.to_string();
    // Fire and forget on a short-lived thread so a slow notification daemon never blocks us.
    let _ = std::thread::Builder::new()
        .name("notify".into())
        .spawn(move || {
            let r = (|| -> zbus::Result<()> {
                let conn = zbus::blocking::Connection::session()?;
                let hints: HashMap<&str, Value<'_>> = HashMap::new();
                conn.call_method(
                    Some("org.freedesktop.Notifications"),
                    "/org/freedesktop/Notifications",
                    Some("org.freedesktop.Notifications"),
                    "Notify",
                    &(
                        "Vaani",
                        0u32,
                        "audio-input-microphone",
                        summary.as_str(),
                        body.as_str(),
                        Vec::<&str>::new(),
                        hints,
                        5000i32,
                    ),
                )?;
                Ok(())
            })();
            if let Err(e) = r {
                tracing::debug!("notification failed: {e}");
            }
        });
}
