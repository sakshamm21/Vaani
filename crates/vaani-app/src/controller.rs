//! The dictation controller: turns hotkey presses and CLI/D-Bus commands into
//! record -> transcribe -> clean -> save -> paste.
//!
//! It runs on its own thread. Slow work (transcription, pasting) runs on a job thread so the
//! hotkey stays responsive and Esc can cancel. The UI reads [`Shared`] to draw itself.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, select, tick};
use vaani_audio::{Level, Recorder, Recording};
use vaani_core::config::{Config, EngineKind, HotkeyMode};
use vaani_core::state::{Command, Effect, Machine, State};
use vaani_hotkey::HotkeyEvent;
use vaani_inject::{Injector, Outcome};
use vaani_store::History;
use vaani_stt::SttEngine;
use vaani_stt::cloud::CloudEngine;
use vaani_stt::local::LocalEngine;

/// A quick tap shorter than this starts hands-free dictation instead of push-to-talk.
const TAP: Duration = Duration::from_millis(300);
/// Clips shorter than this are treated as accidental presses.
const MIN_AUDIO_SAMPLES: usize = 16_000 * 3 / 10;
/// Stop recording automatically after this long.
const MAX_RECORDING: Duration = Duration::from_secs(5 * 60);

pub enum Input {
    Hotkey(HotkeyEvent),
    Cmd(Command),
    PasteLast,
    Config(Box<Config>),
    /// Reload the speech engine (e.g. after downloading a model).
    ReloadEngine,
    JobDone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineStatus {
    Loading(String),
    Ready(String),
    Missing(String),
    Failed(String),
}

/// State the UI reads.
pub struct Shared {
    pub state: Mutex<State>,
    pub engine: Mutex<EngineStatus>,
    pub level: Level,
    /// When the current recording started (for the overlay timer).
    pub recording_since: Mutex<Option<Instant>>,
    /// Hands-free mode (tapped instead of held).
    pub hands_free: AtomicBool,
    /// Last finished dictation, for the overlay's brief "done" flash.
    pub last_text: Mutex<Option<(Instant, String)>>,
    pub config: Mutex<Config>,
}

impl Shared {
    pub fn state(&self) -> State {
        self.state.lock().map(|s| *s).unwrap_or(State::Idle)
    }
    pub fn engine(&self) -> EngineStatus {
        self.engine
            .lock()
            .map(|s| s.clone())
            .unwrap_or(EngineStatus::Failed("lock".into()))
    }
}

type EngineSlot = Arc<Mutex<Option<Box<dyn SttEngine>>>>;

pub struct Controller {
    config: Config,
    shared: Arc<Shared>,
    machine: Machine,
    recorder: Recorder,
    recording: Option<Recording>,
    engine: EngineSlot,
    engine_generation: Arc<std::sync::atomic::AtomicU64>,
    injector: Option<Injector>,
    history: Option<Arc<Mutex<History>>>,
    hotkey: Option<vaani_hotkey::Listener>,
    down_at: Option<Instant>,
    latched: bool,
    job_cancel: Option<Arc<AtomicBool>>,
    tx: Sender<Input>,
}

pub fn new_shared(config: &Config) -> Arc<Shared> {
    Arc::new(Shared {
        state: Mutex::new(State::Idle),
        engine: Mutex::new(EngineStatus::Loading(String::new())),
        level: Level::default(),
        recording_since: Mutex::new(None),
        hands_free: AtomicBool::new(false),
        last_text: Mutex::new(None),
        config: Mutex::new(config.clone()),
    })
}

impl Controller {
    pub fn spawn(config: Config, shared: Arc<Shared>, tx: Sender<Input>, rx: Receiver<Input>) {
        let _ = thread::Builder::new()
            .name("controller".into())
            .spawn(move || {
                let mut c = Controller::new(config, shared, tx);
                c.run(rx);
            });
    }

    fn new(config: Config, shared: Arc<Shared>, tx: Sender<Input>) -> Self {
        let recorder = Recorder::with_level(shared.level.clone());

        let injector = match Injector::start() {
            Ok(i) => Some(i),
            Err(e) => {
                tracing::error!("text insertion unavailable: {e}");
                crate::notify::send("Vaani can't type text", &format!("{e}"));
                None
            }
        };
        let history = vaani_core::paths::data_dir()
            .map(|d| d.join("history.db"))
            .and_then(|p| match History::open(&p) {
                Ok(h) => {
                    if let Some(keep) = config.privacy.history_limit() {
                        let _ = h.trim(keep);
                    }
                    Some(Arc::new(Mutex::new(h)))
                }
                Err(e) => {
                    tracing::error!("history unavailable: {e}");
                    None
                }
            });

        let mut c = Self {
            config,
            shared,
            machine: Machine::new(),
            recorder,
            recording: None,
            engine: Arc::new(Mutex::new(None)),
            engine_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            injector,
            history,
            hotkey: None,
            down_at: None,
            latched: false,
            job_cancel: None,
            tx,
        };
        c.start_hotkey();
        c.load_engine();
        c
    }

    fn start_hotkey(&mut self) {
        let (htx, hrx) = crossbeam_channel::unbounded();
        match vaani_hotkey::listen(&self.config.hotkey.key, htx) {
            Ok(l) => {
                self.hotkey = Some(l);
                let tx = self.tx.clone();
                let _ = thread::Builder::new()
                    .name("hotkey-fwd".into())
                    .spawn(move || {
                        for ev in hrx {
                            if tx.send(Input::Hotkey(ev)).is_err() {
                                break;
                            }
                        }
                    });
            }
            Err(e) => {
                tracing::error!("hotkey unavailable: {e}");
                crate::notify::send(
                    "Vaani hotkey isn't working",
                    &format!(
                        "{e}. You can still bind `vaani toggle` to a shortcut in your desktop settings."
                    ),
                );
            }
        }
    }

    /// Load the configured engine in the background; dictation waits for it if needed.
    fn load_engine(&mut self) {
        let slot = self.engine.clone();
        let shared = self.shared.clone();
        let generation = self.engine_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let current = self.engine_generation.clone();
        let cfg = self.config.engine.clone();
        // Drop the old engine (and its worker) right away so memory is freed before reloading.
        if let Ok(mut s) = slot.lock() {
            *s = None;
        }

        if cfg.kind == EngineKind::Cloud {
            let Some(provider) = vaani_stt::cloud::provider(&cfg.cloud_provider) else {
                self.set_engine(EngineStatus::Failed(format!(
                    "unknown provider {}",
                    cfg.cloud_provider
                )));
                return;
            };
            self.set_engine(EngineStatus::Loading(provider.name.into()));
            let _ = thread::Builder::new()
                .name("engine-load".into())
                .spawn(move || {
                    let key = vaani_store::secrets::get_key(provider.id).unwrap_or_default();
                    let (engine, status): (Option<Box<dyn SttEngine>>, _) =
                        if key.is_empty() && provider.id != "openai_compatible" {
                            (
                                None,
                                EngineStatus::Missing(format!("{} API key", provider.name)),
                            )
                        } else {
                            match CloudEngine::new(
                                provider,
                                &cfg.cloud_base_url,
                                &cfg.cloud_model,
                                &key,
                            ) {
                                Ok(e) => {
                                    let name = e.name();
                                    (Some(Box::new(e)), EngineStatus::Ready(name))
                                }
                                Err(e) => (None, EngineStatus::Failed(e.to_string())),
                            }
                        };
                    if current.load(Ordering::SeqCst) == generation {
                        if let Ok(mut s) = slot.lock() {
                            *s = engine;
                        }
                        if let Ok(mut e) = shared.engine.lock() {
                            *e = status;
                        }
                    }
                });
            return;
        }

        let id = cfg.local_model.clone();
        let Some(model) = vaani_models::find(&id) else {
            self.set_engine(EngineStatus::Failed(format!("unknown model {id}")));
            return;
        };
        if !model.is_installed() {
            self.set_engine(EngineStatus::Missing(model.name.clone()));
            return;
        }
        let threads = match cfg.threads {
            0 => vaani_models::default_threads() as u32,
            n => n,
        };
        self.set_engine(EngineStatus::Loading(model.name.clone()));
        let _ = thread::Builder::new().name("engine-load".into()).spawn(move || {
            let mut engine = LocalEngine::new(model.clone(), threads);
            let status = match engine.warm_up() {
                Ok(d) => {
                    tracing::info!(model = %model.id, ms = d.as_millis() as u64, "speech engine ready");
                    EngineStatus::Ready(model.name.clone())
                }
                Err(e) => {
                    tracing::error!("speech engine failed to load: {e}");
                    EngineStatus::Failed(e.to_string())
                }
            };
            // A newer load may have started meanwhile; only the latest wins.
            if current.load(Ordering::SeqCst) == generation {
                if let Ok(mut s) = slot.lock() {
                    *s = Some(Box::new(engine));
                }
                if let Ok(mut e) = shared.engine.lock() {
                    *e = status;
                }
            }
        });
    }

    fn set_engine(&self, s: EngineStatus) {
        if let Ok(mut e) = self.shared.engine.lock() {
            *e = s;
        }
    }

    fn set_state(&self) {
        if let Ok(mut s) = self.shared.state.lock() {
            *s = self.machine.state();
        }
    }

    fn run(&mut self, rx: Receiver<Input>) {
        let ticker = tick(Duration::from_millis(250));
        loop {
            select! {
                recv(rx) -> msg => match msg {
                    Ok(input) => self.handle(input),
                    Err(_) => return,
                },
                recv(ticker) -> _ => self.on_tick(),
            }
        }
    }

    fn on_tick(&mut self) {
        if let Some(r) = &self.recording {
            if r.elapsed() > MAX_RECORDING {
                tracing::info!(
                    "recording hit the {}-minute limit",
                    MAX_RECORDING.as_secs() / 60
                );
                self.command(Command::Stop);
            }
        }
    }

    fn handle(&mut self, input: Input) {
        match input {
            Input::Hotkey(ev) => self.on_hotkey(ev),
            Input::Cmd(cmd) => self.command(cmd),
            Input::JobDone => {
                self.job_cancel = None;
                self.machine.handle(Command::Finished);
                self.set_state();
            }
            Input::PasteLast => self.paste_last(),
            Input::ReloadEngine => self.load_engine(),
            Input::Config(new) => self.apply_config(*new),
        }
    }

    fn apply_config(&mut self, new: Config) {
        let key_changed = new.hotkey.key != self.config.hotkey.key;
        let engine_changed = new.engine != self.config.engine;
        self.config = new;
        if let Ok(mut c) = self.shared.config.lock() {
            *c = self.config.clone();
        }
        if key_changed {
            match &self.hotkey {
                Some(l) => {
                    if let Err(e) = l.set_key(&self.config.hotkey.key) {
                        tracing::warn!("{e}");
                    }
                }
                None => self.start_hotkey(),
            }
        }
        if engine_changed {
            self.load_engine();
        }
    }

    fn on_hotkey(&mut self, ev: HotkeyEvent) {
        let state = self.machine.state();
        match (self.config.hotkey.mode, ev) {
            (_, HotkeyEvent::Escape) => {
                if state != State::Idle {
                    self.command(Command::Cancel);
                }
            }
            (HotkeyMode::Toggle, HotkeyEvent::Down) => self.command(Command::Toggle),
            (HotkeyMode::Toggle, HotkeyEvent::Up) => {}
            (HotkeyMode::PushToTalk, HotkeyEvent::Down) => match state {
                State::Idle => {
                    self.down_at = Some(Instant::now());
                    self.latched = false;
                    self.command(Command::Start);
                }
                State::Recording if self.latched => self.command(Command::Stop),
                _ => {}
            },
            (HotkeyMode::PushToTalk, HotkeyEvent::Up) => {
                if state == State::Recording && !self.latched {
                    let held = self.down_at.map(|t| t.elapsed()).unwrap_or_default();
                    if held < TAP {
                        self.latched = true;
                        self.shared.hands_free.store(true, Ordering::Relaxed);
                        tracing::debug!("tap: hands-free dictation");
                    } else {
                        self.command(Command::Stop);
                    }
                }
            }
        }
    }

    pub fn command(&mut self, cmd: Command) {
        let effect = self.machine.handle(cmd);
        match effect {
            Effect::None => {}
            Effect::StartCapture => match self.recorder.start(Some(&self.config.audio.device)) {
                Ok(r) => {
                    self.recording = Some(r);
                    if let Ok(mut t) = self.shared.recording_since.lock() {
                        *t = Some(Instant::now());
                    }
                    crate::sounds::play(&self.config, crate::sounds::Cue::Start);
                }
                Err(e) => {
                    tracing::error!("{e}");
                    crate::notify::send("Microphone problem", &e.to_string());
                    self.machine.handle(Command::Cancel);
                }
            },
            Effect::StopCaptureAndProcess => {
                self.clear_recording_flags();
                crate::sounds::play(&self.config, crate::sounds::Cue::Stop);
                match self.recording.take().map(|r| r.finish()) {
                    Some(Ok(samples)) => self.spawn_job(samples),
                    Some(Err(e)) => {
                        tracing::error!("{e}");
                        self.machine.handle(Command::Finished);
                    }
                    None => {
                        self.machine.handle(Command::Finished);
                    }
                }
            }
            Effect::DiscardCapture => {
                self.clear_recording_flags();
                crate::sounds::play(&self.config, crate::sounds::Cue::Cancel);
                if let Some(r) = self.recording.take() {
                    r.discard();
                }
                tracing::info!("dictation cancelled");
            }
            Effect::AbortProcessing => {
                crate::sounds::play(&self.config, crate::sounds::Cue::Cancel);
                if let Some(c) = &self.job_cancel {
                    c.store(true, Ordering::Relaxed);
                }
                tracing::info!("dictation cancelled while processing");
            }
        }
        self.set_state();
    }

    fn clear_recording_flags(&mut self) {
        self.latched = false;
        self.down_at = None;
        self.shared.hands_free.store(false, Ordering::Relaxed);
        if let Ok(mut t) = self.shared.recording_since.lock() {
            *t = None;
        }
    }

    fn spawn_job(&mut self, samples: Vec<f32>) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.job_cancel = Some(cancel.clone());
        let ctx = Job {
            engine: self.engine.clone(),
            injector: self.injector.clone(),
            history: self.history.clone(),
            config: self.config.clone(),
            shared: self.shared.clone(),
            cancel,
        };
        let tx = self.tx.clone();
        let _ = thread::Builder::new()
            .name("dictation".into())
            .spawn(move || {
                ctx.run(samples);
                let _ = tx.send(Input::JobDone);
            });
    }

    fn paste_last(&mut self) {
        let Some(h) = &self.history else { return };
        let last = h.lock().ok().and_then(|h| h.last().ok().flatten());
        if let (Some(e), Some(i)) = (last, &self.injector) {
            match i.insert(&e.text) {
                Ok(outcome) => tracing::info!(?outcome, "paste-last"),
                Err(err) => tracing::warn!("paste-last failed: {err}"),
            }
        }
    }
}

struct Job {
    engine: EngineSlot,
    injector: Option<Injector>,
    history: Option<Arc<Mutex<History>>>,
    config: Config,
    shared: Arc<Shared>,
    cancel: Arc<AtomicBool>,
}

impl Job {
    /// Transcribe with the configured local model when the cloud engine failed.
    fn local_fallback(&self, samples: &[f32]) -> Option<(String, String)> {
        if self.config.engine.kind != EngineKind::Cloud {
            return None;
        }
        let model =
            vaani_models::find(&self.config.engine.local_model).filter(|m| m.is_installed())?;
        tracing::warn!(model = %model.id, "cloud transcription failed; using the local model");
        let threads = vaani_models::default_threads() as u32;
        let mut engine = LocalEngine::new(model, threads);
        match engine.transcribe(samples, &self.config.general.language) {
            Ok(t) => {
                crate::notify::send(
                    "Cloud unavailable",
                    "Vaani used the local model for this dictation.",
                );
                Some((t.text, engine.name()))
            }
            Err(e) => {
                tracing::error!("local fallback failed too: {e}");
                None
            }
        }
    }

    fn run(self, samples: Vec<f32>) {
        let released = Instant::now();
        let audio_ms = (samples.len() as u64 * 1000 / 16_000) as i64;
        if samples.len() < MIN_AUDIO_SAMPLES {
            tracing::debug!(audio_ms, "too short; ignored");
            return;
        }
        // A dictation made while the model is still loading waits for it instead of being
        // dropped. The pill keeps showing "Transcribing" meanwhile.
        let wait_until = Instant::now() + Duration::from_secs(120);
        while matches!(self.shared.engine(), EngineStatus::Loading(_))
            && Instant::now() < wait_until
            && !self.cancel.load(Ordering::Relaxed)
        {
            thread::sleep(Duration::from_millis(100));
        }
        if self.cancel.load(Ordering::Relaxed) {
            return;
        }
        let result = {
            let Ok(mut slot) = self.engine.lock() else {
                return;
            };
            let Some(engine) = slot.as_mut() else {
                let msg = match self.shared.engine() {
                    EngineStatus::Missing(m) => {
                        format!("Download the {m} model in Vaani settings first.")
                    }
                    EngineStatus::Failed(e) => e,
                    _ => "The speech model didn't finish loading.".into(),
                };
                crate::notify::send("Vaani can't transcribe yet", &msg);
                return;
            };
            engine
                .transcribe(&samples, &self.config.general.language)
                .map(|t| (t, engine.name()))
        };
        let (raw, engine_name) = match result {
            Ok((t, name)) => {
                tracing::info!(audio_ms, took_ms = t.took.as_millis() as u64, "transcribed");
                (t.text, name)
            }
            Err(e) => match self.local_fallback(&samples) {
                // The cloud failed (offline, quota, outage): use the local model if one is
                // installed, so the words are not lost.
                Some(done) => done,
                None => {
                    tracing::error!("transcription failed: {e}");
                    crate::notify::send("Transcription failed", &e.to_string());
                    return;
                }
            },
        };
        let rules = vaani_text::Rules {
            remove_fillers: self.config.cleanup.remove_fillers,
            trailing_space: self.config.insert.trailing_space,
        };
        let Some(text) = vaani_text::clean(&raw, &rules) else {
            tracing::debug!("nothing to insert");
            return;
        };
        let app = self
            .injector
            .as_ref()
            .map(|i| i.focused_app())
            .unwrap_or_default();

        // Never lose words: save before typing.
        let limit = self.config.privacy.history_limit();
        let id = if limit == Some(0) {
            None
        } else {
            self.history
                .as_ref()
                .and_then(|h| h.lock().ok())
                .and_then(|h| {
                    let id = h.add(&text, &raw, &engine_name, audio_ms, &app).ok();
                    if let Some(keep) = limit {
                        let _ = h.trim(keep);
                    }
                    id
                })
        };
        if self.cancel.load(Ordering::Relaxed) {
            return;
        }
        let Some(injector) = &self.injector else {
            crate::notify::send("Couldn't type the text", "It's saved in Vaani history.");
            return;
        };
        let mode = match self.config.insert.method {
            vaani_core::config::InsertMethod::Auto => vaani_inject::Mode::Auto,
            vaani_core::config::InsertMethod::Paste => vaani_inject::Mode::Paste,
            vaani_core::config::InsertMethod::Type => vaani_inject::Mode::Type,
        };
        let done = |how: &str, app: &str| {
            let latency = released.elapsed();
            tracing::info!(%app, how, latency_ms = latency.as_millis() as u64, "inserted");
            if let (Some(h), Some(id)) = (&self.history, id) {
                if let Ok(h) = h.lock() {
                    let _ = h.mark_inserted(id, latency.as_millis() as i64);
                }
            }
            if let Ok(mut l) = self.shared.last_text.lock() {
                *l = Some((Instant::now(), text.clone()));
            }
        };
        match injector.insert_with(&text, mode) {
            Ok(Outcome::Typed { app, chars }) => {
                tracing::debug!(chars, "typed");
                done("typed", &app);
            }
            Ok(Outcome::Pasted {
                after,
                terminal,
                app,
            }) => {
                tracing::debug!(terminal, paste_ms = after.as_millis() as u64, "pasted");
                done("pasted", &app);
            }
            Ok(Outcome::LeftOnClipboard { app }) => {
                let where_ = if app.is_empty() {
                    "this window".to_string()
                } else {
                    app
                };
                crate::notify::send(
                    "Press Ctrl+V to paste",
                    &format!(
                        "Vaani couldn't paste into {where_}, so the text is on your clipboard."
                    ),
                );
            }
            Err(e) => {
                tracing::error!("insertion failed: {e}");
                injector.set_clipboard(&text);
                crate::notify::send(
                    "Press Ctrl+V to paste",
                    "Typing failed, so the text is on your clipboard.",
                );
            }
        }
    }
}
