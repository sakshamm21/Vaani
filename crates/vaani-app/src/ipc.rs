//! Talking to the running app: CLI commands (`vaani toggle`, `vaani status`, ...) and the
//! single-instance check. Linux uses D-Bus (see `dbus`), Windows a named pipe (see `pipe`).

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossbeam_channel::Sender;
use vaani_core::state::Command;

use crate::controller::{EngineStatus, Input, Shared};

#[cfg(target_os = "linux")]
pub use crate::dbus::{not_running, running, serve, try_call};
#[cfg(windows)]
pub use crate::pipe::{not_running, running, serve, try_call};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Call {
    Toggle,
    Start,
    Stop,
    Cancel,
    Status,
    PasteLast,
    Command,
    ReloadEngine,
    ShowSettings,
    Quit,
}

// The word protocol is used by the Windows pipe (and the tests).
#[cfg_attr(not(windows), allow(dead_code))]
impl Call {
    pub const ALL: [Call; 10] = [
        Call::Toggle,
        Call::Start,
        Call::Stop,
        Call::Cancel,
        Call::Status,
        Call::PasteLast,
        Call::Command,
        Call::ReloadEngine,
        Call::ShowSettings,
        Call::Quit,
    ];

    /// The word sent over the pipe.
    pub fn name(self) -> &'static str {
        match self {
            Call::Toggle => "toggle",
            Call::Start => "start",
            Call::Stop => "stop",
            Call::Cancel => "cancel",
            Call::Status => "status",
            Call::PasteLast => "paste-last",
            Call::Command => "command",
            Call::ReloadEngine => "reload-engine",
            Call::ShowSettings => "show-settings",
            Call::Quit => "quit",
        }
    }

    pub fn parse(word: &str) -> Option<Call> {
        Self::ALL.into_iter().find(|c| c.name() == word.trim())
    }

    /// Whether the call prints something.
    pub fn has_output(self) -> bool {
        matches!(
            self,
            Call::Toggle | Call::Start | Call::Stop | Call::Cancel | Call::Status | Call::Command
        )
    }
}

/// Do what `call` asks in the running app. Returns the text to print, if any: the state
/// after the call ("idle", "recording" or "processing") or the status line.
pub fn handle(call: Call, tx: &Sender<Input>, shared: &Arc<Shared>) -> Option<String> {
    let apply = |input: Input| {
        let _ = tx.send(input);
        // Let the controller act so the reply shows the new state.
        std::thread::sleep(Duration::from_millis(40));
        Some(shared.state().to_string())
    };
    match call {
        Call::Toggle => apply(Input::Cmd(Command::Toggle)),
        Call::Start => apply(Input::Cmd(Command::Start)),
        Call::Stop => apply(Input::Cmd(Command::Stop)),
        Call::Cancel => apply(Input::Cmd(Command::Cancel)),
        Call::Command => apply(Input::CommandToggle),
        Call::Status => {
            let engine = match shared.engine() {
                EngineStatus::Ready(m) => format!("ready ({m})"),
                EngineStatus::Loading(m) => format!("loading {m}"),
                EngineStatus::Missing(m) => format!("model {m} not downloaded"),
                EngineStatus::Failed(e) => format!("error: {e}"),
            };
            Some(format!("{}; engine {engine}", shared.state()))
        }
        Call::PasteLast => {
            let _ = tx.send(Input::PasteLast);
            None
        }
        Call::ReloadEngine => {
            let _ = tx.send(Input::ReloadEngine);
            None
        }
        Call::ShowSettings => {
            crate::ui::request_settings();
            None
        }
        Call::Quit => {
            tracing::info!("quit requested");
            crate::ui::request_quit();
            None
        }
    }
}

/// Run one CLI command against the running app and print the result.
pub fn client_call(call: Call) -> Result<ExitCode> {
    crate::logging::init_cli();
    match try_call(call) {
        Ok(Some(out)) => {
            println!("{out}");
            Ok(ExitCode::SUCCESS)
        }
        Ok(None) => Ok(ExitCode::SUCCESS),
        Err(e) if not_running(&e) => {
            eprintln!("Vaani is not running. Start it with `vaani` (or from the app menu).");
            Ok(ExitCode::from(3))
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_names_round_trip() {
        for c in Call::ALL {
            assert_eq!(Call::parse(c.name()), Some(c));
        }
        assert_eq!(Call::parse("status\n"), Some(Call::Status));
        assert_eq!(Call::parse("nope"), None);
    }
}
