//! `vaani`: one binary for the background app and the command line.

mod autostart;
mod cli;
mod controller;
mod daemon;
mod dbus;
mod doctor;
mod keys;
mod logging;
mod media;
mod models_cmd;
mod notify;
mod overlay;
mod settings;
mod sounds;
mod style;
mod tray;
mod ui;

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Cmd, ModelsCmd};
use dbus::Call;

fn main() -> ExitCode {
    lighter_rendering();
    let cli = Cli::parse();
    let result = match cli.command.unwrap_or(Cmd::Run) {
        Cmd::Run => daemon::run(false),
        Cmd::Toggle => dbus::client_call(Call::Toggle),
        Cmd::Start => dbus::client_call(Call::Start),
        Cmd::Stop => dbus::client_call(Call::Stop),
        Cmd::Cancel => dbus::client_call(Call::Cancel),
        Cmd::Status => dbus::client_call(Call::Status),
        Cmd::PasteLast => dbus::client_call(Call::PasteLast),
        Cmd::Settings => match dbus::try_call(Call::ShowSettings) {
            // Opened from the app menu while Vaani isn't running: start it with the window open.
            Err(e) if dbus::not_running(&e) => daemon::run(true),
            Err(e) => Err(e),
            Ok(_) => Ok(ExitCode::SUCCESS),
        },
        Cmd::Quit => dbus::client_call(Call::Quit),
        Cmd::Models { action } => match action.unwrap_or(ModelsCmd::List) {
            ModelsCmd::List => models_cmd::list(),
            ModelsCmd::Download { id, r#use } => models_cmd::download(&id, r#use),
            ModelsCmd::Remove { id } => models_cmd::remove(&id),
        },
        Cmd::History { count } => models_cmd::history(count),
        Cmd::Doctor { json } => doctor::run(json),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("vaani: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Vaani's UI is a few flat widgets, so GTK's software cairo renderer is plenty and skips a
/// GL context: about 90 MB less memory while the settings window is open. Users can still
/// pick a renderer with GSK_RENDERER.
#[allow(unsafe_code)]
fn lighter_rendering() {
    if std::env::var_os("GSK_RENDERER").is_none() {
        // SAFETY: called first thing in main, before any other thread exists.
        unsafe { std::env::set_var("GSK_RENDERER", "cairo") };
    }
}
