//! Local models, run in the `vaani-worker` child process.
//!
//! The worker keeps the model loaded between dictations (Parakeet takes about 4 s to load).
//! If it crashes or is killed for memory, the next call restarts it and retries once.

use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vaani_models::ModelInfo;

use crate::protocol::{self, Request, Response};
use crate::{SttEngine, SttError, Transcript};

struct Worker {
    child: Arc<Mutex<Child>>,
    tx: BufWriter<ChildStdin>,
    rx: BufReader<ChildStdout>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = protocol::write(&mut self.tx, &Request::Shutdown);
        if let Ok(mut c) = self.child.lock() {
            let _ = c.wait();
        }
    }
}

/// How long a transcription may take before the worker is taken to be hung: generous, since
/// a large Whisper model on a slow CPU can run slower than real time.
pub fn transcribe_limit(samples_16k: usize) -> Duration {
    Duration::from_secs(30) + Duration::from_millis(samples_16k as u64 * 3000 / 16_000)
}

/// Kills the worker if the reply has not come by the deadline, so a hung model can never
/// block dictation for good. Disarmed when dropped.
struct Watchdog {
    done: Arc<(Mutex<bool>, std::sync::Condvar)>,
    fired: Arc<AtomicBool>,
}

impl Watchdog {
    fn arm(child: Arc<Mutex<Child>>, limit: Duration) -> Self {
        let done = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let fired = Arc::new(AtomicBool::new(false));
        let (d, f) = (done.clone(), fired.clone());
        let _ = std::thread::Builder::new()
            .name("worker-watchdog".into())
            .spawn(move || {
                let (lock, cv) = &*d;
                let Ok(g) = lock.lock() else { return };
                let Ok((g, _)) = cv.wait_timeout_while(g, limit, |finished| !*finished) else {
                    return;
                };
                if !*g {
                    f.store(true, Ordering::SeqCst);
                    tracing::error!(secs = limit.as_secs(), "speech worker hung; stopping it");
                    if let Ok(mut c) = child.lock() {
                        let _ = c.kill();
                    }
                }
            });
        Self { done, fired }
    }

    fn fired(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        let (lock, cv) = &*self.done;
        if let Ok(mut g) = lock.lock() {
            *g = true;
        }
        cv.notify_all();
    }
}

pub struct LocalEngine {
    model: ModelInfo,
    threads: u32,
    worker_path: PathBuf,
    worker: Option<Worker>,
    language: String,
}

/// On Windows the worker is built into `vaani.exe`, which starts a copy of itself with this
/// first argument. One program for application control policies (Smart App Control) to allow.
pub const WORKER_ARG: &str = "--speech-worker";

/// The program to start as the worker: on Windows the running `vaani.exe` itself.
#[cfg(windows)]
pub fn worker_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("vaani.exe"))
}

/// `vaani-worker` next to the running binary, else on PATH.
#[cfg(not(windows))]
pub fn worker_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| {
            p.parent()
                .map(|d| d.join(format!("vaani-worker{}", std::env::consts::EXE_SUFFIX)))
        })
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
            language: String::new(),
        }
    }

    /// The spoken language setting, so the model loads ready for it.
    pub fn with_language(mut self, language: &str) -> Self {
        self.language = language.to_string();
        self
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
        let mut cmd = Command::new(&self.worker_path);
        #[cfg(windows)]
        {
            cmd.arg(WORKER_ARG);
            // No console window for the worker.
            use std::os::windows::process::CommandExt as _;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd
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
            child: Arc::new(Mutex::new(child)),
            tx: BufWriter::new(stdin),
            rx: BufReader::new(stdout),
        };
        protocol::write(
            &mut w.tx,
            &Request::Load {
                kind: self.model.kind,
                dir: dir.display().to_string(),
                threads: self.threads,
                language: self.worker_language(&self.language),
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

    /// The language the worker should force. Only multilingual Whisper models take one
    /// (others detect it, or know just one language); empty means detect it.
    fn worker_language(&self, language: &str) -> String {
        if self.model.kind != vaani_models::ModelKind::Whisper || !self.model.is_multilingual() {
            return String::new();
        }
        crate::base_language(language)
            .unwrap_or_default()
            .to_string()
    }

    pub fn is_ready(&self) -> bool {
        self.worker.is_some()
    }

    fn try_transcribe(&mut self, samples: &[f32], language: &str) -> Result<Transcript, SttError> {
        if self.worker.is_none() {
            self.warm_up()?;
        }
        let language = self.worker_language(language);
        let w = self
            .worker
            .as_mut()
            .ok_or_else(|| SttError::NotReady("worker not running".into()))?;
        let watchdog = Watchdog::arm(w.child.clone(), transcribe_limit(samples.len()));
        protocol::write(
            &mut w.tx,
            &Request::Transcribe {
                samples: samples.to_vec(),
                language,
            },
        )?;
        let reply = protocol::read::<Response>(&mut w.rx);
        if watchdog.fired() {
            self.worker = None;
            return Err(SttError::Engine(
                "the speech model stopped responding; it will restart for the next dictation"
                    .into(),
            ));
        }
        drop(watchdog);
        match reply? {
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

    /// Recording started: if the model was freed while idle, load it again now, so it is
    /// ready (Parakeet takes about 2 s) by the time the key is let go.
    fn warm(&mut self) {
        if self.worker.is_none() {
            if let Err(e) = self.warm_up() {
                tracing::warn!("speech model failed to load: {e}");
            }
        }
    }

    fn unload(&mut self) -> bool {
        let freed = self.worker.take().is_some();
        if freed {
            tracing::info!(model = %self.model.id, "speech model unloaded while idle");
        }
        freed
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
