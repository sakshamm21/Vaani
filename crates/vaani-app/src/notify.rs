//! Desktop notifications through org.freedesktop.Notifications (works on every desktop).

use std::collections::HashMap;

use zbus::zvariant::Value;

pub fn send(summary: &str, body: &str) {
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
