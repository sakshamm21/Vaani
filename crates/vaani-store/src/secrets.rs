//! API keys in the system keyring (GNOME Keyring / KWallet via the Secret Service), never in
//! the config file.

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
