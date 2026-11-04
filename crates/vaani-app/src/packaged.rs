//! Windows: running from the Microsoft Store (MSIX) package.
//!
//! A packaged app has its own app ID, which Windows uses for its notifications, and starts at
//! sign-in through the package's StartupTask (Run key entries written from inside a package
//! are private to it, so Explorer never sees them).

#![allow(unsafe_code)]

use std::sync::OnceLock;

use windows::ApplicationModel::Activation::ActivationKind;
use windows::ApplicationModel::{AppInstance, StartupTask, StartupTaskState};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Storage::Packaging::Appx::GetCurrentApplicationUserModelId;
use windows::core::{HSTRING, PWSTR};

/// The StartupTask's id in AppxManifest.xml.
const STARTUP_TASK: &str = "VaaniStartup";

/// The package's app ID ("<family name>!Vaani"), or None outside a package.
pub fn app_id() -> Option<&'static str> {
    static ID: OnceLock<Option<String>> = OnceLock::new();
    ID.get_or_init(|| {
        let mut buf = [0u16; 256];
        let mut len = buf.len() as u32;
        // SAFETY: the buffer and its length describe valid stack memory.
        let r =
            unsafe { GetCurrentApplicationUserModelId(&mut len, Some(PWSTR(buf.as_mut_ptr()))) };
        (r == ERROR_SUCCESS)
            .then(|| String::from_utf16_lossy(&buf[..len.saturating_sub(1) as usize]))
    })
    .as_deref()
}

/// Turn the package's start-at-sign-in task on or off to match `enabled`.
pub fn sync_startup(enabled: bool) {
    let r = (|| -> windows::core::Result<()> {
        let task = StartupTask::GetAsync(&HSTRING::from(STARTUP_TASK))?.join()?;
        let state = task.State()?;
        if enabled {
            if state == StartupTaskState::Disabled {
                let now = task.RequestEnableAsync()?.join()?;
                if now != StartupTaskState::Enabled {
                    tracing::info!(?now, "start at sign-in was turned off in Windows Settings");
                }
            }
        } else if state == StartupTaskState::Enabled {
            task.Disable()?;
        }
        Ok(())
    })();
    if let Err(e) = r {
        tracing::warn!("could not change start at sign-in: {e}");
    }
}

/// Started from the package's Start menu entry (not at sign-in, not from the `vaani` command).
pub fn launched_from_start_menu() -> bool {
    app_id().is_some()
        && AppInstance::GetActivatedEventArgs()
            .and_then(|a| a.Kind())
            .is_ok_and(|k| k == ActivationKind::Launch)
}
