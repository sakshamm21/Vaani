//! `vaani`: one binary for the background app and the command line.

mod cli;
mod controller;
mod daemon;
mod dbus;
mod doctor;
mod logging;
mod models_cmd;
mod notify;
mod sounds;
mod ui;

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Cmd, ModelsCmd};
use dbus::Call;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command.unwrap_or(Cmd::Run) {
        Cmd::Run => daemon::run(),
        Cmd::Toggle => dbus::client_call(Call::Toggle),
        Cmd::Start => dbus::client_call(Call::Start),
        Cmd::Stop => dbus::client_call(Call::Stop),
        Cmd::Cancel => dbus::client_call(Call::Cancel),
        Cmd::Status => dbus::client_call(Call::Status),
        Cmd::PasteLast => dbus::client_call(Call::PasteLast),
        Cmd::Settings => dbus::client_call(Call::ShowSettings),
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
