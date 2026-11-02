//! Local models, run in the `vaani-worker` child process.
//!
//! The worker keeps the model loaded between dictations (Parakeet takes about 4 s to load).
//! If it crashes or is killed for memory, the next call restarts it and retries once.

use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

use vaani_models::ModelInfo;

use crate::protocol::{self, Request, Response};
use crate::{SttEngine, SttError, Transcript};

struct Worker {
    child: Child,
    tx: BufWriter<ChildStdin>,
    rx: BufReader<ChildStdout>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = protocol::write(&mut self.tx, &Request::Shutdown);
        let _ = self.child.wait();
    }
}

pub struct LocalEngine {
    model: ModelInfo,
    threads: u32,
    worker_path: PathBuf,
    worker: Option<Worker>,
}

/// `vaani-worker` next to the running binary, else on PATH.
pub fn worker_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("vaani-worker")))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("vaani-worker"))
}

impl LocalEngine {
    pub fn new(model: ModelInfo, threads: u32) -> Self {
        Self {
            model,
            threads,
            worker_path: worker_path(),
            worker: None,
        }
    }

    pub fn model(&self) -> &ModelInfo {
        &self.model
    }

    /// Start the worker and load the model now, so the first dictation is fast.
    pub fn warm_up(&mut self) -> Result<Duration, SttError> {
        self.worker = None;
        let dir = self
            .model
            .dir()
            .filter(|_| self.model.is_installed())
            .ok_or_else(|| {
                SttError::NotReady(format!("model {} is not downloaded", self.model.id))
            })?;
        let mut child = Command::new(&self.worker_path)
            .arg("--serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                SttError::NotReady(format!("cannot start {}: {e}", self.worker_path.display()))
            })?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(SttError::NotReady("worker pipes unavailable".into()));
        };
        let mut w = Worker {
            child,
            tx: BufWriter::new(stdin),
            rx: BufReader::new(stdout),
        };
        protocol::write(
            &mut w.tx,
            &Request::Load {
                kind: self.model.kind,
                dir: dir.display().to_string(),
                threads: self.threads,
            },
        )?;
        match protocol::read::<Response>(&mut w.rx)? {
            Response::Loaded { ms } => {
                tracing::info!(model = %self.model.id, ms, threads = self.threads, "model loaded in worker");
                self.worker = Some(w);
                Ok(Duration::from_millis(ms))
            }
            Response::Error(e) => Err(SttError::Engine(e)),
            other => Err(SttError::Engine(format!("unexpected reply {other:?}"))),
        }
    }

    pub fn is_ready(&self) -> bool {
        self.worker.is_some()
    }

    fn try_transcribe(&mut self, samples: &[f32], language: &str) -> Result<Transcript, SttError> {
        if self.worker.is_none() {
            self.warm_up()?;
        }
        let w = self
            .worker
            .as_mut()
            .ok_or_else(|| SttError::NotReady("worker not running".into()))?;
        protocol::write(
            &mut w.tx,
            &Request::Transcribe {
                samples: samples.to_vec(),
                language: language.to_string(),
            },
        )?;
        match protocol::read::<Response>(&mut w.rx)? {
            Response::Text { text, ms } => Ok(Transcript {
                text,
                took: Duration::from_millis(ms),
            }),
            Response::Error(e) => Err(SttError::Engine(e)),
            other => Err(SttError::Engine(format!("unexpected reply {other:?}"))),
        }
    }
}

impl SttEngine for LocalEngine {
    fn name(&self) -> String {
        format!("{} (local)", self.model.name)
    }

    fn transcribe(&mut self, samples: &[f32], language: &str) -> Result<Transcript, SttError> {
        match self.try_transcribe(samples, language) {
            Err(e @ (SttError::Io(_) | SttError::Protocol(_))) => {
                // The worker crashed or was killed for memory. Restart it once and retry.
                tracing::warn!("speech worker failed ({e}); restarting it");
                self.worker = None;
                self.try_transcribe(samples, language)
            }
            other => other,
        }
    }
}
