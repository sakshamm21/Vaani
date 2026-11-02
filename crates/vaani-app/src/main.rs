//! `vaani`: one binary for the background app and the command line.
//!
//! `vaani` (or `vaani run`) starts the app. `vaani toggle`, `start`, `stop`, `cancel` and `status`
//! talk to the running app over D-Bus, so any desktop's custom-shortcut settings can drive it.

mod cli;
mod daemon;
mod dbus;
mod doctor;
mod logging;

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Cmd};

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command.unwrap_or(Cmd::Run) {
        Cmd::Run => daemon::run(),
        Cmd::Toggle => dbus::client_call(dbus::Call::Toggle),
        Cmd::Start => dbus::client_call(dbus::Call::Start),
        Cmd::Stop => dbus::client_call(dbus::Call::Stop),
        Cmd::Cancel => dbus::client_call(dbus::Call::Cancel),
        Cmd::Status => dbus::client_call(dbus::Call::Status),
        Cmd::Quit => dbus::client_call(dbus::Call::Quit),
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
