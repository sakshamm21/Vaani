//! `vaani doctor`: a report on this machine's setup, written to be pasted into bug reports.
//!
//! Each component reports its own state here as it is built ("definition of done").
//! The report never includes transcripts, API keys or file contents.

use std::process::ExitCode;

use anyhow::Result;
use serde::Serialize;
use vaani_core::config::Config;
use vaani_platform::{SessionInfo, SessionType};

#[derive(Serialize)]
struct Report {
    version: &'static str,
    os: String,
    session: SessionInfo,
    config: ConfigReport,
    log_dir: Option<String>,
    worker: Check,
    running: bool,
    components: Vec<Component>,
}

#[derive(Serialize)]
struct ConfigReport {
    path: Option<String>,
    exists: bool,
    status: Check,
}

#[derive(Serialize)]
struct Check {
    ok: bool,
    detail: String,
}

#[derive(Serialize)]
struct Component {
    name: &'static str,
    status: &'static str,
}

/// Components not built yet. Each moves out of this list when its milestone lands.
const PLANNED: &[(&str, &str)] = &[
    ("audio capture", "planned for M1"),
    ("speech-to-text", "planned for M1"),
    ("hotkey", "planned for M1"),
    ("text insertion", "planned for M1"),
    ("overlay", "planned for M1"),
    ("tray", "planned for M1"),
];

pub fn run(json: bool) -> Result<ExitCode> {
    crate::logging::init_cli();
    let report = collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_text(&report);
    }
    Ok(if report.config.status.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn collect() -> Report {
    let config_path = vaani_core::paths::config_file();
    let config = match &config_path {
        None => ConfigReport {
            path: None,
            exists: false,
            status: Check {
                ok: false,
                detail: "no home directory found".into(),
            },
        },
        Some(p) => {
            let exists = p.exists();
            // Read without saving a migration back: doctor must not change files.
            let status = match std::fs::read_to_string(p) {
                Err(_) if !exists => Check {
                    ok: true,
                    detail: "no file yet, using defaults".into(),
                },
                Err(e) => Check {
                    ok: false,
                    detail: format!("unreadable: {e}"),
                },
                Ok(text) => match Config::from_toml(&text, p) {
                    Ok((_, false)) => Check {
                        ok: true,
                        detail: "valid".into(),
                    },
                    Ok((_, true)) => Check {
                        ok: true,
                        detail: "valid, will be migrated on next start".into(),
                    },
                    Err(e) => Check {
                        ok: false,
                        detail: e.to_string(),
                    },
                },
            };
            ConfigReport {
                path: Some(p.display().to_string()),
                exists,
                status,
            }
        }
    };

    Report {
        version: env!("CARGO_PKG_VERSION"),
        os: os_release(),
        session: vaani_platform::probe(),
        config,
        log_dir: vaani_core::paths::log_dir().map(|p| p.display().to_string()),
        worker: worker_check(),
        running: running(),
        components: PLANNED
            .iter()
            .map(|(name, status)| Component { name, status })
            .collect(),
    }
}

fn os_release() -> String {
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                .map(|v| v.trim_matches('"').to_string())
        })
        .unwrap_or_else(|| std::env::consts::OS.to_string())
}

/// Look for `vaani-worker` next to this binary, then on PATH.
fn worker_check() -> Check {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("vaani-worker")))
        .filter(|p| p.is_file());
    let found = beside.or_else(|| {
        std::env::var("PATH").ok().and_then(|path| {
            path.split(':')
                .map(|d| std::path::Path::new(d).join("vaani-worker"))
                .find(|p| p.is_file())
        })
    });
    match found {
        Some(p) => Check {
            ok: true,
            detail: p.display().to_string(),
        },
        None => Check {
            ok: false,
            detail: "not found next to vaani or on PATH".into(),
        },
    }
}

fn running() -> bool {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return false;
    };
    rt.block_on(async {
        match zbus::Connection::session().await {
            Ok(conn) => crate::dbus::is_running(&conn).await,
            Err(_) => false,
        }
    })
}

fn yes(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

fn print_text(r: &Report) {
    let s = &r.session;
    println!("Vaani doctor (version {})", r.version);
    println!();
    println!("System");
    println!("  OS:              {}", r.os);
    let session = match s.session_type {
        SessionType::X11 => "X11",
        SessionType::Wayland if s.xwayland => "Wayland (XWayland available)",
        SessionType::Wayland => "Wayland",
        SessionType::Tty => "text console (no display)",
        SessionType::Unknown => "unknown",
    };
    println!("  Session:         {session}");
    println!(
        "  Desktop:         {:?} ({})",
        s.desktop,
        s.desktop_raw
            .as_deref()
            .unwrap_or("XDG_CURRENT_DESKTOP not set")
    );
    println!("  D-Bus session:   {}", yes(s.dbus_session_bus));
    println!(
        "  /dev/uinput:     {}",
        if s.uinput_writable {
            "writable"
        } else {
            "not writable"
        }
    );
    println!(
        "  evdev devices:   {}",
        if s.evdev_readable {
            "readable"
        } else {
            "not readable"
        }
    );
    let tools: Vec<String> = s
        .tools
        .iter()
        .map(|(n, found)| format!("{n} {}", if *found { "found" } else { "missing" }))
        .collect();
    println!("  Helper tools:    {}", tools.join(", "));
    println!();
    println!("Vaani");
    println!("  Running:         {}", yes(r.running));
    println!(
        "  Config:          {} ({})",
        r.config.path.as_deref().unwrap_or("?"),
        r.config.status.detail
    );
    println!("  Logs:            {}", r.log_dir.as_deref().unwrap_or("?"));
    println!(
        "  Worker:          {}",
        if r.worker.ok {
            r.worker.detail.clone()
        } else {
            format!("problem: {}", r.worker.detail)
        }
    );
    println!();
    println!("Components");
    for c in &r.components {
        println!("  {:<16} {}", format!("{}:", c.name), c.status);
    }
}
