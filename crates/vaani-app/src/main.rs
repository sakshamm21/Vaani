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

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Cmd, ModelsCmd};
use ipc::Call;

fn main() -> ExitCode {
    #[cfg(windows)]
    attach_console();
    lighter_rendering();
    let cli = Cli::parse();
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
#[cfg(not(windows))]
#[allow(unsafe_code)]
fn lighter_rendering() {
    if std::env::var_os("GSK_RENDERER").is_none() {
        // SAFETY: called first thing in main, before any other thread exists.
        unsafe { std::env::set_var("GSK_RENDERER", "cairo") };
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
