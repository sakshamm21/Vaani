//! `vaani run`: the background app. GTK (overlay, settings) runs on the main thread; the
//! IPC server (D-Bus on Linux, a named pipe on Windows), the Linux tray and signal handling
//! run on a small tokio runtime; the controller has its own thread, and so does the Windows
//! tray.

use std::process::ExitCode;

use anyhow::{Context, Result};
use vaani_core::config::{Config, ConfigWatcher};

use crate::controller::{Controller, Input};

pub fn run(show_settings: bool) -> Result<ExitCode> {
    let config_path = vaani_core::paths::config_file().context("no home directory found")?;
    let loaded = Config::load(&config_path);
    let level = loaded
        .as_ref()
        .map(|c| c.logging.level.clone())
        .unwrap_or_else(|_| "info".into());
    let logging = crate::logging::init_daemon(&level);
    let config = match loaded {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("{e}; using defaults until the file is fixed");
            crate::notify::send(
                "Vaani settings file has a problem",
                &format!("{e}. Using defaults."),
            );
            Config::default()
        }
    };
    let first_run = !config_path.exists();
    if first_run {
        let _ = config.save(&config_path);
    }
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        config = %config_path.display(),
        logs = ?logging.dir,
        "starting Vaani"
    );
    #[cfg(windows)]
    crate::notify::register();
    crate::autostart::sync(config.general.autostart);
    let session = vaani_platform::probe();
    tracing::info!(session = ?session.session_type, desktop = ?session.desktop, "session probed");

    let shared = crate::controller::new_shared(&config);
    let (tx, rx) = crossbeam_channel::unbounded();
    let ui_rx = crate::ui::channel();

    // Claim the IPC name first: a second launch just opens the running app's settings.
    let (claimed_tx, claimed_rx) = std::sync::mpsc::channel::<Result<bool, String>>();
    {
        let shared = shared.clone();
        let tx = tx.clone();
        std::thread::Builder::new()
            .name("tokio".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = claimed_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                rt.block_on(async move {
                    let conn = match crate::ipc::serve(shared.clone(), tx.clone()).await {
                        Ok(Some(c)) => {
                            let _ = claimed_tx.send(Ok(true));
                            c
                        }
                        Ok(None) => {
                            let _ = claimed_tx.send(Ok(false));
                            return;
                        }
                        Err(e) => {
                            let _ = claimed_tx.send(Err(format!("{e:#}")));
                            return;
                        }
                    };
                    crate::tray::start(shared, tx);
                    wait_for_signal().await;
                    crate::ui::request_quit();
                    let _keep = conn;
                });
            })?;
    }
    match claimed_rx.recv() {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => {
            eprintln!("Vaani is already running; opening its settings.");
            let _ = crate::ipc::try_call(crate::ipc::Call::ShowSettings);
            return Ok(ExitCode::SUCCESS);
        }
        Ok(Err(e)) => anyhow::bail!(e),
        Err(_) => anyhow::bail!("IPC thread failed to start"),
    }
    tracing::info!("listening for commands");

    let needs_setup =
        vaani_models::find(&config.engine.local_model).is_none_or(|m| !m.is_installed());
    Controller::spawn(config.clone(), shared.clone(), tx.clone(), rx);
    crate::update::spawn(shared.clone());

    let watch_tx = tx.clone();
    let _watcher = ConfigWatcher::spawn(config_path, config, move |res| match res {
        Ok(c) => {
            tracing::info!("config reloaded");
            crate::autostart::sync(c.general.autostart);
            let _ = watch_tx.send(Input::Config(Box::new(c)));
        }
        Err(e) => {
            tracing::warn!("config change ignored: {e}");
            crate::notify::send("Vaani settings not applied", &e.to_string());
        }
    })
    .map_err(|e| tracing::warn!("config hot reload unavailable: {e}"))
    .ok();

    crate::ui::run(shared, tx, ui_rx, first_run || needs_setup || show_settings);
    tracing::info!("Vaani stopped");
    // Worker and helper threads end with the process.
    std::process::exit(0);
}

/// Wait for Ctrl+C (and SIGTERM on Linux).
async fn wait_for_signal() {
    #[cfg(unix)]
    {
        let Ok(mut sigterm) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        else {
            return std::future::pending().await;
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => tracing::info!("interrupted"),
            _ = sigterm.recv() => tracing::info!("terminated"),
        }
    }
    #[cfg(not(unix))]
    {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::info!("interrupted");
        } else {
            std::future::pending::<()>().await;
        }
    }
}
