//! `vaani run`: the background app.

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use tokio::sync::Notify;
use vaani_core::config::{Config, ConfigWatcher};
use vaani_core::state::Machine;
use vaani_core::{DBUS_NAME, DBUS_PATH};

use crate::dbus::Service;

pub fn run() -> Result<ExitCode> {
    let config_path = vaani_core::paths::config_file().context("no home directory found")?;
    // Load before logging starts so the configured level applies; report problems after.
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
            Config::default()
        }
    };
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
    let machine = Arc::new(Mutex::new(Machine::new()));
    let quit = Arc::new(Notify::new());

    let conn = zbus::connection::Builder::session()
        .context("could not connect to the D-Bus session bus")?
        .serve_at(DBUS_PATH, Service::new(machine.clone(), quit.clone()))?
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

    let _watcher = ConfigWatcher::spawn(config_path, config, |res| match res {
        Ok(_) => tracing::info!("config reloaded"),
        Err(e) => tracing::warn!("config change ignored: {e}"),
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
