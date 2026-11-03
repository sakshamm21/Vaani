//! Linux: the D-Bus service the running app owns, and the client side used by CLI commands.
//!
//! Owning the well-known name is also how we stay single-instance.

use std::sync::Arc;

use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use vaani_core::{DBUS_NAME, DBUS_PATH};

use crate::controller::{Input, Shared};
use crate::ipc::{Call, handle};

pub struct Service {
    tx: Sender<Input>,
    shared: Arc<Shared>,
}

impl Service {
    pub fn new(tx: Sender<Input>, shared: Arc<Shared>) -> Self {
        Self { tx, shared }
    }

    fn call(&self, call: Call) -> String {
        handle(call, &self.tx, &self.shared).unwrap_or_default()
    }
}

#[zbus::interface(name = "io.github.sakshamm21.Vaani1")]
impl Service {
    /// Each method returns the state after the call: "idle", "recording" or "processing".
    fn toggle(&self) -> String {
        self.call(Call::Toggle)
    }
    fn start(&self) -> String {
        self.call(Call::Start)
    }
    fn stop(&self) -> String {
        self.call(Call::Stop)
    }
    fn cancel(&self) -> String {
        self.call(Call::Cancel)
    }
    fn status(&self) -> String {
        self.call(Call::Status)
    }
    fn paste_last(&self) {
        self.call(Call::PasteLast);
    }
    fn command(&self) -> String {
        self.call(Call::Command)
    }
    fn reload_engine(&self) {
        self.call(Call::ReloadEngine);
    }
    fn show_settings(&self) {
        self.call(Call::ShowSettings);
    }
    fn quit(&self) {
        self.call(Call::Quit);
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

/// Whether another instance is running (for `vaani doctor`).
pub fn running() -> bool {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return false;
    };
    rt.block_on(async {
        match zbus::Connection::session().await {
            Ok(conn) => is_running(&conn).await,
            Err(_) => false,
        }
    })
}

/// Serve the D-Bus API. Ok(None) if another instance owns the name.
pub async fn serve(shared: Arc<Shared>, tx: Sender<Input>) -> Result<Option<zbus::Connection>> {
    let conn = zbus::connection::Builder::session()
        .context("could not connect to the D-Bus session bus")?
        .serve_at(DBUS_PATH, Service::new(tx, shared))?
        .build()
        .await
        .context("could not connect to the D-Bus session bus")?;
    match conn
        .request_name_with_flags(DBUS_NAME, zbus::fdo::RequestNameFlags::DoNotQueue.into())
        .await
    {
        Ok(_) => Ok(Some(conn)),
        Err(zbus::Error::NameTaken) => Ok(None),
        Err(e) => Err(e).context("could not claim the D-Bus name"),
    }
}

/// Is another instance already serving the D-Bus name?
async fn is_running(conn: &zbus::Connection) -> bool {
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
