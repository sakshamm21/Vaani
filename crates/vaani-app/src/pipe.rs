//! Windows: the named pipe the running app serves, and the client side used by CLI
//! commands. Being the first to create the pipe is also how we stay single-instance.
//!
//! The protocol is one line each way: the command word (see [`Call::name`]), then the
//! reply (empty for calls with no output).

use std::sync::Arc;

use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};

use crate::controller::{Input, Shared};
use crate::ipc::{Call, handle};

/// ERROR_FILE_NOT_FOUND: nobody serves the pipe.
const NOT_FOUND: i32 = 2;
/// ERROR_ACCESS_DENIED: the pipe already exists (another instance created it first).
const ACCESS_DENIED: i32 = 5;
/// ERROR_PIPE_BUSY: the server is between two connections.
const PIPE_BUSY: i32 = 231;
/// How long a CLI command waits for the running app's reply.
const REPLY_LIMIT: std::time::Duration = std::time::Duration::from_secs(10);

/// One pipe per user, so two people signed in on one PC each get their own Vaani.
fn name() -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    let user: String = user
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!(r"\\.\pipe\io.github.sakshamm21.Vaani-{user}")
}

#[derive(Debug, thiserror::Error)]
#[error("Vaani is not running")]
struct NotRunning;

/// True when a call failed only because Vaani isn't running.
pub fn not_running(e: &anyhow::Error) -> bool {
    e.downcast_ref::<NotRunning>().is_some()
}

/// Whether another instance is running (for `vaani doctor`).
pub fn running() -> bool {
    match ClientOptions::new().open(name()) {
        Ok(_) => true,
        Err(e) => e.raw_os_error() == Some(PIPE_BUSY),
    }
}

/// Call the running app; Ok(None) for calls with no output.
pub fn try_call(call: Call) -> Result<Option<String>> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let mut tries = 0;
        let pipe = loop {
            match ClientOptions::new().open(name()) {
                Ok(p) => break p,
                Err(e) if e.raw_os_error() == Some(NOT_FOUND) => {
                    return Err(anyhow::Error::new(NotRunning));
                }
                Err(e) if e.raw_os_error() == Some(PIPE_BUSY) && tries < 50 => {
                    tries += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                Err(e) => return Err(e).context("could not reach the running Vaani"),
            }
        };
        let talk = async move {
            let (rd, mut wr) = tokio::io::split(pipe);
            wr.write_all(format!("{}\n", call.name()).as_bytes())
                .await?;
            wr.flush().await?;
            let mut line = String::new();
            BufReader::new(rd).read_line(&mut line).await?;
            anyhow::Ok(line.trim_end().to_string())
        };
        // A reply never takes long; don't let a stuck app hang the command forever.
        let line = tokio::time::timeout(REPLY_LIMIT, talk)
            .await
            .map_err(|_| anyhow::anyhow!("the running Vaani did not answer"))??;
        Ok(call.has_output().then_some(line))
    })
}

/// A guard for the served pipe; the server task stops with the runtime.
pub struct Served;

/// Serve the pipe. Ok(None) if another instance already serves it.
pub async fn serve(shared: Arc<Shared>, tx: Sender<Input>) -> Result<Option<Served>> {
    let first = match ServerOptions::new()
        .first_pipe_instance(true)
        .reject_remote_clients(true)
        .create(name())
    {
        Ok(s) => s,
        Err(e) if e.raw_os_error() == Some(ACCESS_DENIED) => return Ok(None),
        Err(e) => return Err(e).context("could not create the Vaani pipe"),
    };
    tokio::spawn(async move {
        let mut server = first;
        loop {
            if let Err(e) = server.connect().await {
                tracing::warn!("pipe connect failed: {e}");
            }
            let connected = server;
            // Make the next instance before serving this one, so clients never find no pipe.
            server = match ServerOptions::new()
                .reject_remote_clients(true)
                .create(name())
            {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("could not keep serving the Vaani pipe: {e}");
                    return;
                }
            };
            let shared = shared.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                let (rd, mut wr) = tokio::io::split(connected);
                let mut line = String::new();
                if BufReader::new(rd).read_line(&mut line).await.is_err() {
                    return;
                }
                let reply = match Call::parse(&line) {
                    Some(call) => tokio::task::spawn_blocking(move || handle(call, &tx, &shared))
                        .await
                        .ok()
                        .flatten()
                        .unwrap_or_default(),
                    None => format!("unknown command `{}`", line.trim()),
                };
                let _ = wr.write_all(format!("{reply}\n").as_bytes()).await;
                let _ = wr.flush().await;
            });
        }
    });
    Ok(Some(Served))
}
