//! `vaani run`: the background app.

use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::Notify;
use vaani_core::config::{Config, ConfigWatcher};
use vaani_core::{DBUS_NAME, DBUS_PATH};

use crate::controller::{Controller, Input};
use crate::dbus::Service;

pub fn run() -> Result<ExitCode> {
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
    // Write a config file on first run so people can find and edit it.
    if !config_path.exists() {
        let _ = config.save(&config_path);
    }
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        config = %config_path.display(),
        logs = ?logging.dir,
        "starting Vaani"
    );
    let session = vaani_platform::probe();
    tracing::info!(session = ?session.session_type, desktop = ?session.desktop, "session probed");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    rt.block_on(serve(config_path, config))
}

async fn serve(config_path: std::path::PathBuf, config: Config) -> Result<ExitCode> {
    let quit = Arc::new(Notify::new());
    let shared = crate::controller::new_shared(&config);
    let (tx, rx) = crossbeam_channel::unbounded();

    let conn = zbus::connection::Builder::session()
        .context("could not connect to the D-Bus session bus")?
        .serve_at(
            DBUS_PATH,
            Service::new(tx.clone(), shared.clone(), quit.clone()),
        )?
        .build()
        .await
        .context("could not connect to the D-Bus session bus")?;
    match conn
        .request_name_with_flags(DBUS_NAME, zbus::fdo::RequestNameFlags::DoNotQueue.into())
        .await
    {
        Ok(_) => {}
        Err(zbus::Error::NameTaken) => {
            eprintln!("Vaani is already running.");
            return Ok(ExitCode::from(3));
        }
        Err(e) => return Err(e).context("could not claim the D-Bus name"),
    }
    tracing::info!(name = DBUS_NAME, "listening on D-Bus");

    Controller::spawn(config.clone(), shared.clone(), tx.clone(), rx);

    let watch_tx = tx.clone();
    let _watcher = ConfigWatcher::spawn(config_path, config, move |res| match res {
        Ok(c) => {
            tracing::info!("config reloaded");
            let _ = watch_tx.send(Input::Config(Box::new(c)));
        }
        Err(e) => {
            tracing::warn!("config change ignored: {e}");
            crate::notify::send("Vaani settings not applied", &e.to_string());
        }
    })
    .map_err(|e| tracing::warn!("config hot reload unavailable: {e}"))
    .ok();

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => tracing::info!("interrupted"),
        _ = sigterm.recv() => tracing::info!("terminated"),
        _ = quit.notified() => {}
    }
    tracing::info!("Vaani stopped");
    Ok(ExitCode::SUCCESS)
}
