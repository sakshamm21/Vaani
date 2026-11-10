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
    dictations: Option<Dictations>,
}

/// Delay and failures of the most recent dictations, from history (numbers only, no text).
#[derive(Serialize)]
struct Dictations {
    count: usize,
    /// Saved but never typed or pasted into an app.
    not_inserted: usize,
    /// Key release to text inserted, in milliseconds.
    p50_ms: i64,
    p95_ms: i64,
    /// The same for clips of 10 seconds or less, the build plan's yardstick.
    short_p95_ms: i64,
}

/// The value at fraction `q` (0..1) of sorted values.
fn quantile(sorted: &[i64], q: f64) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let i = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

fn dictations() -> Option<Dictations> {
    let path = vaani_core::paths::data_dir()?.join("history.db");
    if !path.exists() {
        return None;
    }
    let h = vaani_store::History::open(&path).ok()?;
    let recent = h.recent(200).ok()?;
    if recent.is_empty() {
        return None;
    }
    let mut all: Vec<i64> = recent
        .iter()
        .filter(|e| e.inserted && e.latency_ms > 0)
        .map(|e| e.latency_ms)
        .collect();
    all.sort_unstable();
    let mut short: Vec<i64> = recent
        .iter()
        .filter(|e| e.inserted && e.latency_ms > 0 && e.audio_ms <= 10_000)
        .map(|e| e.latency_ms)
        .collect();
    short.sort_unstable();
    Some(Dictations {
        count: recent.len(),
        not_inserted: recent.iter().filter(|e| !e.inserted).count(),
        p50_ms: quantile(&all, 0.5),
        p95_ms: quantile(&all, 0.95),
        short_p95_ms: quantile(&short, 0.95),
    })
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
    ok: bool,
    status: String,
}

fn component(name: &'static str, r: Result<String, String>) -> Component {
    match r {
        Ok(status) => Component {
            name,
            ok: true,
            status,
        },
        Err(status) => Component {
            name,
            ok: false,
            status,
        },
    }
}

fn components(session: &SessionInfo) -> Vec<Component> {
    let config = vaani_core::paths::config_file()
        .and_then(|p| Config::load(&p).ok())
        .unwrap_or_default();
    let mut v = Vec::new();
    let key = &config.hotkey.key;
    v.push(component(
        "hotkey",
        match vaani_hotkey::backend() {
            vaani_hotkey::Backend::Hook => Ok(format!("keyboard hook, key {key}")),
            vaani_hotkey::Backend::X11 if session.x11_display.is_some() => {
                Ok(format!("X11 XInput2, key {key}"))
            }
            vaani_hotkey::Backend::X11 => Err("no X display to listen on".into()),
            vaani_hotkey::Backend::Evdev => Ok(format!("evdev (/dev/input), key {key}")),
            vaani_hotkey::Backend::Portal if vaani_hotkey::portal_available() => Ok(
                "GlobalShortcuts portal (Esc can't cancel; to hold any key such as Right Ctrl, \
                 run `sudo usermod -aG input $USER` and log in again)"
                    .into(),
            ),
            vaani_hotkey::Backend::Portal => Err(
                "can't read /dev/input and the desktop has no GlobalShortcuts portal: run \
                 `sudo usermod -aG input $USER` and log in again, or bind `vaani toggle` to a \
                 shortcut in your desktop settings"
                    .into(),
            ),
        },
    ));
    let mics = vaani_audio::input_devices();
    v.push(component(
        "microphone",
        if mics.is_empty() {
            Err("no input devices found".into())
        } else if config.audio.device.is_empty() {
            Ok(format!("system default ({} available)", mics.len()))
        } else if mics.contains(&config.audio.device) {
            Ok(config.audio.device.clone())
        } else {
            Err(format!(
                "configured mic `{}` not found",
                config.audio.device
            ))
        },
    ));
    v.push(component(
        "speech model",
        match vaani_models::find(&config.engine.local_model) {
            None => Err(format!("unknown model `{}`", config.engine.local_model)),
            Some(m) if m.is_installed() => Ok(format!("{} (downloaded)", m.name)),
            Some(m) => Err(format!(
                "{} not downloaded: vaani models download {}",
                m.name, m.id
            )),
        },
    ));
    v.push(component(
        "text insertion",
        match (session.session_type, session.x11_display.is_some()) {
            (SessionType::Windows, _) => {
                Ok("clipboard paste via SendInput, typing as fallback".into())
            }
            (SessionType::Wayland, _)
                if vaani_inject::wayland_clipboard() && vaani_inject::uinput_usable() =>
            {
                Ok("Wayland data-control clipboard, keys via a uinput virtual keyboard".into())
            }
            (_, false) => Err("needs XWayland or an X11 display for the clipboard".into()),
            (SessionType::Wayland, true) if vaani_inject::uinput_usable() => {
                Ok("clipboard via XWayland, keys via a uinput virtual keyboard".into())
            }
            (SessionType::Wayland, true) => Err(
                "can't open /dev/uinput, so keys only reach XWayland apps; see the README's \
                 Wayland section"
                    .into(),
            ),
            _ => Ok("clipboard paste via XTest".into()),
        },
    ));
    v
}

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
    let mut r = collect_base();
    r.components = components(&r.session);
    r.dictations = dictations();
    r
}

fn collect_base() -> Report {
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
        components: Vec::new(),
        dictations: None,
    }
}

#[cfg(windows)]
fn os_release() -> String {
    // e.g. "Windows 11 Pro 24H2 (build 26100)". ProductName still says "Windows 10" on
    // Windows 11, so the build number decides (11 starts at 22000).
    let Ok(k) =
        windows_registry::LOCAL_MACHINE.open(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion")
    else {
        return "Windows".into();
    };
    let product = k
        .get_string("ProductName")
        .unwrap_or_else(|_| "Windows".into());
    let build = k.get_string("CurrentBuild").unwrap_or_default();
    let display = k.get_string("DisplayVersion").unwrap_or_default();
    let product = if build.parse::<u32>().is_ok_and(|b| b >= 22000) {
        product.replace("Windows 10", "Windows 11")
    } else {
        product
    };
    format!("{product} {display} (build {build})").replace("  ", " ")
}

#[cfg(not(windows))]
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

/// On Windows the speech worker is part of vaani.exe.
#[cfg(windows)]
fn worker_check() -> Check {
    Check {
        ok: true,
        detail: "built into vaani.exe".into(),
    }
}

#[cfg(not(windows))]
const WORKER: &str = "vaani-worker";

/// Look for `vaani-worker` next to this binary, then on PATH.
#[cfg(not(windows))]
fn worker_check() -> Check {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(WORKER)))
        .filter(|p| p.is_file());
    let found = beside.or_else(|| {
        std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|d| d.join(WORKER))
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
    crate::ipc::running()
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
        SessionType::Windows => "Windows desktop",
        SessionType::Unknown => "unknown",
    };
    println!("  Session:         {session}");
    if s.session_type == SessionType::Windows {
        print_vaani(r);
        return;
    }
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
    print_vaani(r);
}

fn print_vaani(r: &Report) {
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
    if let Some(d) = &r.dictations {
        println!(
            "  Last {} dictations: {} ms typical, {} ms slowest 5% ({} ms for clips up to 10 s), {} not inserted",
            d.count, d.p50_ms, d.p95_ms, d.short_p95_ms, d.not_inserted
        );
    }
    println!();
    println!("Components");
    for c in &r.components {
        println!(
            "  {:<16} {} {}",
            format!("{}:", c.name),
            if c.ok { "ok  " } else { "FAIL" },
            c.status
        );
    }
}
