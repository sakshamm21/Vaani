//! The D-Bus service the running app owns, and the client side used by CLI commands.
//!
//! Owning the well-known name is also how we stay single-instance.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use vaani_core::state::Command;

use crate::controller::{EngineStatus, Input, Shared};

pub struct Service {
    tx: Sender<Input>,
    shared: Arc<Shared>,
}

impl Service {
    pub fn new(tx: Sender<Input>, shared: Arc<Shared>) -> Self {
        Self { tx, shared }
    }

    fn apply(&self, cmd: Command) -> String {
        let _ = self.tx.send(Input::Cmd(cmd));
        // Let the controller act so the reply shows the new state.
        std::thread::sleep(Duration::from_millis(40));
        self.shared.state().to_string()
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
        let engine = match self.shared.engine() {
            EngineStatus::Ready(m) => format!("ready ({m})"),
            EngineStatus::Loading(m) => format!("loading {m}"),
            EngineStatus::Missing(m) => format!("model {m} not downloaded"),
            EngineStatus::Failed(e) => format!("error: {e}"),
        };
        format!("{}; engine {engine}", self.shared.state())
    }
    fn paste_last(&self) {
        let _ = self.tx.send(Input::PasteLast);
    }
    fn command(&self) -> String {
        let _ = self.tx.send(Input::CommandToggle);
        std::thread::sleep(Duration::from_millis(40));
        self.shared.state().to_string()
    }
    fn reload_engine(&self) {
        let _ = self.tx.send(Input::ReloadEngine);
    }
    fn show_settings(&self) {
        crate::ui::request_settings();
    }
    fn quit(&self) {
        tracing::info!("quit requested over D-Bus");
        crate::ui::request_quit();
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
    fn paste_last(&self) -> zbus::Result<()>;
    fn command(&self) -> zbus::Result<String>;
    fn reload_engine(&self) -> zbus::Result<()>;
    fn show_settings(&self) -> zbus::Result<()>;
    fn quit(&self) -> zbus::Result<()>;
}

#[derive(Clone, Copy)]
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

/// Run one CLI command against the running app and print the result.
pub fn client_call(call: Call) -> Result<ExitCode> {
    crate::logging::init_cli();
    match try_call(call) {
        Ok(Some(out)) => {
            println!("{out}");
            Ok(ExitCode::SUCCESS)
        }
        Ok(None) => Ok(ExitCode::SUCCESS),
        Err(e) if e.downcast_ref::<zbus::Error>().is_some_and(is_not_running) => {
            eprintln!("Vaani is not running. Start it with `vaani` (or from the app menu).");
            Ok(ExitCode::from(3))
        }
        Err(e) => Err(e),
    }
}

/// True when a call failed only because Vaani isn't running.
pub fn not_running(e: &anyhow::Error) -> bool {
    e.downcast_ref::<zbus::Error>().is_some_and(is_not_running)
}

/// Call the running app; Ok(None) for calls with no output.
pub fn try_call(call: Call) -> Result<Option<String>> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let conn = zbus::Connection::session()
            .await
            .context("could not connect to the D-Bus session bus")?;
        let proxy = VaaniProxy::new(&conn).await?;
        let r = match call {
            Call::Toggle => proxy.toggle().await.map(Some),
            Call::Start => proxy.start().await.map(Some),
            Call::Stop => proxy.stop().await.map(Some),
            Call::Cancel => proxy.cancel().await.map(Some),
            Call::Status => proxy.status().await.map(Some),
            Call::PasteLast => proxy.paste_last().await.map(|()| None),
            Call::Command => proxy.command().await.map(Some),
            Call::ReloadEngine => proxy.reload_engine().await.map(|()| None),
            Call::ShowSettings => proxy.show_settings().await.map(|()| None),
            Call::Quit => proxy.quit().await.map(|()| None),
        };
        Ok(r?)
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
