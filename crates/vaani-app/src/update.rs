//! The opt-in update check (General → "Tell me when a new version is out", off by default).
//!
//! Once a day while Vaani runs, it asks GitHub for the latest release of
//! github.com/sakshamm21/Vaani and shows one notification per new version. The request
//! carries nothing but Vaani's version in the User-Agent. The Microsoft Store copy skips it:
//! the Store updates it.

use std::sync::Arc;
use std::time::Duration;

use crate::controller::Shared;

const LATEST: &str = "https://api.github.com/repos/sakshamm21/Vaani/releases/latest";
const RELEASES: &str = "https://github.com/sakshamm21/Vaani/releases/latest";
const EVERY: Duration = Duration::from_secs(24 * 60 * 60);

pub fn spawn(shared: Arc<Shared>) {
    #[cfg(windows)]
    if crate::packaged::app_id().is_some() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("update-check".into())
        .spawn(move || {
            // Let start-up finish first.
            std::thread::sleep(Duration::from_secs(60));
            loop {
                let on = shared
                    .config
                    .lock()
                    .map(|c| c.general.check_updates)
                    .unwrap_or(false);
                if on {
                    check();
                }
                // Checked often, acted on daily: turning the setting on takes effect soon.
                std::thread::sleep(Duration::from_secs(10 * 60));
                if on {
                    std::thread::sleep(EVERY.saturating_sub(Duration::from_secs(10 * 60)));
                }
            }
        });
}

fn check() {
    let latest = match fetch_latest() {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("update check failed: {e}");
            return;
        }
    };
    let current = env!("CARGO_PKG_VERSION");
    if !is_newer(&latest, current) {
        tracing::debug!(%latest, "Vaani is up to date");
        return;
    }
    // One notification per version.
    let marker = vaani_core::paths::data_dir().map(|d| d.join("update-notified"));
    if let Some(m) = &marker {
        if std::fs::read_to_string(m).is_ok_and(|v| v.trim() == latest) {
            return;
        }
    }
    tracing::info!(%latest, %current, "a new version of Vaani is available");
    crate::notify::send(
        &format!("Vaani {latest} is available"),
        &format!("You have {current}. Download it from {RELEASES}"),
    );
    if let Some(m) = &marker {
        if let Some(d) = m.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(m, &latest);
    }
}

fn fetch_latest() -> Result<String, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("vaani/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let v: serde_json::Value = client
        .get(LATEST)
        .header("Accept", "application/vnd.github+json")
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .and_then(reqwest::blocking::Response::json)
        .map_err(|e| e.to_string())?;
    v["tag_name"]
        .as_str()
        .map(|t| t.trim_start_matches('v').to_string())
        .ok_or_else(|| "no tag_name in the reply".into())
}

/// Whether version `a` ("0.6.0") is newer than `b` ("0.5.3").
fn is_newer(a: &str, b: &str) -> bool {
    let parse =
        |v: &str| -> Vec<u64> { v.split(['.', '-']).map_while(|p| p.parse().ok()).collect() };
    parse(a) > parse(b)
}

#[cfg(test)]
mod tests {
    #[test]
    fn compares_versions() {
        assert!(super::is_newer("0.5.4", "0.5.3"));
        assert!(super::is_newer("0.10.0", "0.9.9"));
        assert!(super::is_newer("1.0.0", "0.99.0"));
        assert!(!super::is_newer("0.5.3", "0.5.3"));
        assert!(!super::is_newer("0.5.2", "0.5.3"));
    }
}
