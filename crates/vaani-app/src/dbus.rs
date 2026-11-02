//! The D-Bus service the running app owns, and the client side used by CLI commands.
//!
//! Owning the well-known name is also how we stay single-instance: a second `vaani run`
//! finds the name taken and exits.

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use tokio::sync::Notify;
use vaani_core::state::{Command, Effect, Machine};

pub struct Service {
    machine: Arc<Mutex<Machine>>,
    quit: Arc<Notify>,
}

impl Service {
    pub fn new(machine: Arc<Mutex<Machine>>, quit: Arc<Notify>) -> Self {
        Self { machine, quit }
    }

    fn apply(&self, cmd: Command) -> String {
        let Ok(mut m) = self.machine.lock() else {
            return "error".into();
        };
        let effect = m.handle(cmd);
        // M0 has no audio pipeline yet: log what would happen and finish immediately.
        match effect {
            Effect::None => {}
            Effect::StartCapture => tracing::info!("dictation started (no audio yet in M0)"),
            Effect::StopCaptureAndProcess => {
                tracing::info!("dictation finished (no pipeline yet in M0)");
                m.handle(Command::Finished);
            }
            Effect::DiscardCapture | Effect::AbortProcessing => {
                tracing::info!("dictation cancelled");
            }
        }
        m.state().to_string()
    }
}

#[zbus::interface(name = "io.github.sakshamm21.Vaani1")]
impl Service {
    /// Each method returns the state after the call: "idle", "recording" or "processing".
    fn toggle(&self) -> String {
        self.apply(Command::Toggle)
    }
    fn start(&self) -> String {
        self.apply(Command::Start)
    }
    fn stop(&self) -> String {
        self.apply(Command::Stop)
    }
    fn cancel(&self) -> String {
        self.apply(Command::Cancel)
    }
    fn status(&self) -> String {
        self.machine
            .lock()
            .map(|m| m.state().to_string())
            .unwrap_or_else(|_| "error".into())
    }
    fn quit(&self) {
        tracing::info!("quit requested over D-Bus");
        self.quit.notify_one();
    }
    #[zbus(property)]
    fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }
}

#[zbus::proxy(
    interface = "io.github.sakshamm21.Vaani1",
    default_service = "io.github.sakshamm21.Vaani",
    default_path = "/io/github/sakshamm21/Vaani"
)]
trait Vaani {
    fn toggle(&self) -> zbus::Result<String>;
    fn start(&self) -> zbus::Result<String>;
    fn stop(&self) -> zbus::Result<String>;
    fn cancel(&self) -> zbus::Result<String>;
    fn status(&self) -> zbus::Result<String>;
    fn quit(&self) -> zbus::Result<()>;
}

#[derive(Clone, Copy)]
pub enum Call {
    Toggle,
    Start,
    Stop,
    Cancel,
    Status,
    Quit,
}

/// Run one CLI command against the running app and print the resulting state.
pub fn client_call(call: Call) -> Result<ExitCode> {
    crate::logging::init_cli();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let conn = zbus::Connection::session()
            .await
            .context("could not connect to the D-Bus session bus")?;
        let proxy = VaaniProxy::new(&conn).await?;
        let result = match call {
            Call::Toggle => proxy.toggle().await.map(Some),
            Call::Start => proxy.start().await.map(Some),
            Call::Stop => proxy.stop().await.map(Some),
            Call::Cancel => proxy.cancel().await.map(Some),
            Call::Status => proxy.status().await.map(Some),
            Call::Quit => proxy.quit().await.map(|()| None),
        };
        match result {
            Ok(Some(state)) => {
                println!("{state}");
                Ok(ExitCode::SUCCESS)
            }
            Ok(None) => Ok(ExitCode::SUCCESS),
            Err(e) if is_not_running(&e) => {
                eprintln!("Vaani is not running. Start it with `vaani run`.");
                Ok(ExitCode::from(3))
            }
            Err(e) => Err(e.into()),
        }
    })
}

/// Is another instance already serving the D-Bus name?
pub async fn is_running(conn: &zbus::Connection) -> bool {
    match zbus::fdo::DBusProxy::new(conn).await {
        Ok(p) => match zbus::names::BusName::try_from(vaani_core::DBUS_NAME) {
            Ok(name) => p.name_has_owner(name).await.unwrap_or(false),
            Err(_) => false,
        },
        Err(_) => false,
    }
}

fn is_not_running(e: &zbus::Error) -> bool {
    match e {
        zbus::Error::MethodError(name, _, _) => {
            let n = name.as_str();
            n == "org.freedesktop.DBus.Error.ServiceUnknown"
                || n == "org.freedesktop.DBus.Error.NameHasNoOwner"
        }
        zbus::Error::FDO(fdo) => matches!(
            **fdo,
            zbus::fdo::Error::ServiceUnknown(_) | zbus::fdo::Error::NameHasNoOwner(_)
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_applies_commands() {
        let svc = Service::new(
            Arc::new(Mutex::new(Machine::new())),
            Arc::new(Notify::new()),
        );
        assert_eq!(svc.apply(Command::Toggle), "recording");
        // M0 finishes processing immediately.
        assert_eq!(svc.apply(Command::Toggle), "idle");
        assert_eq!(svc.apply(Command::Start), "recording");
        assert_eq!(svc.apply(Command::Cancel), "idle");
    }
}
