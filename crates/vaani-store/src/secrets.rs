//! API keys in the system keyring, never in the config file: GNOME Keyring or KWallet
//! through the Secret Service on Linux, Credential Manager on Windows.

#[cfg(windows)]
mod imp {
    fn entry(provider: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new("vaani", provider).map_err(|e| e.to_string())
    }

    pub fn set_key(provider: &str, key: &str) -> Result<(), String> {
        let e = entry(provider)?;
        if key.is_empty() {
            match e.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(err) => Err(err.to_string()),
            }
        } else {
            e.set_password(key).map_err(|e| e.to_string())
        }
    }

    pub fn get_key(provider: &str) -> Option<String> {
        entry(provider)
            .ok()?
            .get_password()
            .ok()
            .filter(|k| !k.is_empty())
    }
}

#[cfg(windows)]
pub use imp::{get_key, set_key};

#[cfg(not(windows))]
pub use secret_service::{get_key, set_key};

#[cfg(not(windows))]
mod secret_service {

    use std::collections::HashMap;

    fn attrs(provider: &str) -> HashMap<&str, &str> {
        HashMap::from([("application", "vaani"), ("provider", provider)])
    }

    fn block_on<F: std::future::Future>(f: F) -> Option<F::Output> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()
            .map(|rt| rt.block_on(f))
    }

    /// Store (or replace) the API key for `provider`. An empty key deletes it.
    pub fn set_key(provider: &str, key: &str) -> Result<(), String> {
        block_on(async {
            let kr = oo7::Keyring::new().await.map_err(|e| e.to_string())?;
            let _ = kr.unlock().await;
            if key.is_empty() {
                kr.delete(&attrs(provider)).await.map_err(|e| e.to_string())
            } else {
                kr.create_item(
                    &format!("Vaani API key ({provider})"),
                    &attrs(provider),
                    key,
                    true,
                )
                .await
                .map_err(|e| e.to_string())
            }
        })
        .unwrap_or_else(|| Err("could not start keyring client".into()))
    }

    /// The stored API key for `provider`, if any.
    pub fn get_key(provider: &str) -> Option<String> {
        block_on(async {
            let kr = oo7::Keyring::new().await.ok()?;
            let _ = kr.unlock().await;
            let items = kr.search_items(&attrs(provider)).await.ok()?;
            let item = items.into_iter().next()?;
            let _ = item.unlock().await;
            let secret = item.secret().await.ok()?;
            String::from_utf8(secret.as_bytes().to_vec()).ok()
        })
        .flatten()
        .filter(|k| !k.is_empty())
    }
}
