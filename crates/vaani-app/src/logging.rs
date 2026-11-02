//! Logs: rotating daily files in `~/.local/state/vaani/logs` (7 kept), plus stderr.
//!
//! Rule: never log transcript text or API keys at INFO or above.

use std::path::PathBuf;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

/// Keep the returned guard alive for the life of the process, or buffered lines are lost.
pub struct Logging {
    pub dir: Option<PathBuf>,
    _guard: Option<WorkerGuard>,
}

/// Set up logging for the background app. `level` is the config's `logging.level`;
/// the `VAANI_LOG` environment variable overrides it.
pub fn init_daemon(level: &str) -> Logging {
    let filter = || {
        EnvFilter::try_from_env("VAANI_LOG")
            .or_else(|_| EnvFilter::try_new(level))
            .unwrap_or_else(|_| EnvFilter::new("info"))
    };
    let dir = vaani_core::paths::log_dir();
    let file = dir.as_ref().and_then(|d| {
        std::fs::create_dir_all(d).ok()?;
        Builder::new()
            .rotation(Rotation::DAILY)
            .filename_prefix("vaani")
            .filename_suffix("log")
            .max_log_files(7)
            .build(d)
            .ok()
    });

    let stderr_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(filter());
    let (file_layer, guard) = match file {
        Some(appender) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let layer = fmt::layer()
                .with_ansi(false)
                .with_writer(writer)
                .with_filter(filter());
            (Some(layer), Some(guard))
        }
        None => (None, None),
    };
    let _ = tracing_subscriber::registry()
        .with(stderr_layer)
        .with(file_layer)
        .try_init();
    install_panic_hook();
    Logging { dir, _guard: guard }
}

/// Short-lived CLI commands only print warnings and errors to stderr.
pub fn init_cli() {
    let filter = EnvFilter::try_from_env("VAANI_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(filter),
        )
        .try_init();
}

/// Send panics to the log file as well as stderr, so `vaani doctor` reports can include them.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".into());
        tracing::error!(%location, "panic: {payload}");
        default(info);
    }));
}

use tracing_subscriber::Layer as _;
