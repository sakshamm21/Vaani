//! `vaani`: one binary for the background app and the command line.

// A GUI program on Windows, so no console window opens with it; command-line use attaches
// to the terminal it was started from (see `attach_console`).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod ai_cmd;
mod autostart;
mod cli;
mod controller;
mod daemon;
#[cfg(target_os = "linux")]
mod dbus;
mod doctor;
mod ipc;
mod keys;
mod logging;
mod media;
mod models_cmd;
#[cfg(windows)]
mod native_win;
#[cfg(target_os = "linux")]
mod native_x11;
mod notify;
mod overlay;
#[cfg(windows)]
mod packaged;
#[cfg(windows)]
mod pipe;
mod preview;
mod settings;
mod sounds;
mod style;
mod tray;
#[cfg(target_os = "linux")]
mod tray_sni;
#[cfg(windows)]
mod tray_win;
mod ui;
mod update;

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Cmd, ModelsCmd};
use ipc::Call;

fn main() -> ExitCode {
    // Speech worker mode: vaani.exe started by itself (see vaani_stt::local), before anything
    // touches the console or the standard handles, which are the protocol pipes.
    #[cfg(windows)]
    if std::env::args().nth(1).as_deref() == Some(vaani_stt::local::WORKER_ARG) {
        let args: Vec<String> = std::env::args().skip(2).collect();
        std::process::exit(vaani_worker::run(&args));
    }
    #[cfg(windows)]
    attach_console();
    lighter_rendering();
    let cli = Cli::parse();
    // The Store package's Start menu entry starts vaani.exe with no arguments: open Settings.
    #[cfg(windows)]
    let cli = if cli.command.is_none() && packaged::launched_from_start_menu() {
        Cli {
            command: Some(Cmd::Settings),
        }
    } else {
        cli
    };
    let result = match cli.command.unwrap_or(Cmd::Run) {
        Cmd::Run => daemon::run(false),
        Cmd::Toggle => ipc::client_call(Call::Toggle),
        Cmd::Start => ipc::client_call(Call::Start),
        Cmd::Stop => ipc::client_call(Call::Stop),
        Cmd::Cancel => ipc::client_call(Call::Cancel),
        Cmd::Status => ipc::client_call(Call::Status),
        Cmd::PasteLast => ipc::client_call(Call::PasteLast),
        Cmd::Command => ipc::client_call(Call::Command),
        Cmd::Settings => match ipc::try_call(Call::ShowSettings) {
            // Opened from the app menu while Vaani isn't running: start it with the window open.
            Err(e) if ipc::not_running(&e) => daemon::run(true),
            Err(e) => Err(e),
            Ok(_) => Ok(ExitCode::SUCCESS),
        },
        Cmd::Quit => ipc::client_call(Call::Quit),
        Cmd::Models { action } => match action.unwrap_or(ModelsCmd::List) {
            ModelsCmd::List => models_cmd::list(),
            ModelsCmd::Download { id, r#use } => models_cmd::download(&id, r#use),
            ModelsCmd::Remove { id } => models_cmd::remove(&id),
        },
        Cmd::History { count } => models_cmd::history(count),
        Cmd::Doctor { json } => doctor::run(json),
        Cmd::Cleanup { text, file, models } => ai_cmd::run(text, file, models),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("vaani: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Print to the terminal `vaani` was run from, if any. A program built for the Windows GUI
/// subsystem has no console of its own; started from the Start menu it stays silent.
#[cfg(windows)]
#[allow(unsafe_code)]
fn attach_console() {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: a plain call; failing just means there is no parent console.
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

/// Windows: GTK's default renderer, which draws the pill's rounded, see-through edges with
/// the GPU compositor.
#[cfg(windows)]
fn lighter_rendering() {}

/// Vaani's UI is a few flat widgets, so GTK's software cairo renderer is plenty and skips a
/// GL context: about 90 MB less memory while the settings window is open. Users can still
/// pick a renderer with GSK_RENDERER.
///
/// GTK still opens EGL at start-up to see what GL it could use, even with the cairo
/// renderer, which maps Mesa and LLVM into the process: idle, Vaani measured 145 MB resident
/// with it and 68 MB without (Ubuntu 24.04, GTK 4.14). So GL is switched off too, unless the
/// user picked a renderer. GDK_DEBUG=gl-disable is the GTK 4.14 spelling, GDK_DISABLE=gl the
/// one from 4.16 on.
#[cfg(not(windows))]
#[allow(unsafe_code)]
fn lighter_rendering() {
    if std::env::var_os("GSK_RENDERER").is_none() {
        // SAFETY: called first thing in main, before any other thread exists.
        unsafe { std::env::set_var("GSK_RENDERER", "cairo") };
        for (var, value) in [("GDK_DEBUG", "gl-disable"), ("GDK_DISABLE", "gl")] {
            match std::env::var(var) {
                // SAFETY: as above.
                Err(_) => unsafe { std::env::set_var(var, value) },
                Ok(v) if !v.split([',', ':']).any(|x| x == value) => {
                    // SAFETY: as above.
                    unsafe { std::env::set_var(var, format!("{v},{value}")) }
                }
                Ok(_) => {}
            }
        }
    }
    // On Wayland the pill must sit above other windows without taking focus, which plain
    // Wayland windows can't do on every desktop. Through XWayland it is an override-redirect
    // window, so run the UI there when XWayland is available.
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
    let x = std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty());
    if wayland && x && std::env::var_os("GDK_BACKEND").is_none() {
        // SAFETY: as above.
        unsafe { std::env::set_var("GDK_BACKEND", "x11") };
    }
}
