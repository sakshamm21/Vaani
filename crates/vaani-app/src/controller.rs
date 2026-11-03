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
/// Long dictations are transcribed in pieces while the speaker talks, cut at pauses, so the
/// wait after the key is released stays short. A piece is at least this long.
const PIECE_MIN: usize = 16_000 * 15;
/// Past this much untranscribed audio, cut at the quietest moment even without a clear pause.
const PIECE_FORCE: usize = 16_000 * 45;

/// Pieces of a long dictation already transcribed (or in flight) while recording.
#[derive(Default)]
struct Early {
    texts: Vec<String>,
    /// 16 kHz samples covered by `texts`.
    offset: usize,
    /// The piece being transcribed now: where it ends, and its result.
    pending: Option<(usize, Receiver<Result<String, String>>)>,
    last_check: Option<Instant>,
}

impl Early {
    /// Collect a finished piece, if any. `wait` blocks until the in-flight piece is done.
    fn collect(&mut self, wait: bool) {
        let Some((cut, rx)) = &self.pending else {
            return;
        };
        let got = if wait {
            rx.recv_timeout(Duration::from_secs(120)).ok()
        } else {
            rx.try_recv().ok()
        };
        let disconnected = !wait
            && matches!(
                rx.try_recv(),
                Err(crossbeam_channel::TryRecvError::Disconnected)
            );
        match got {
            Some(Ok(text)) => {
                let cut = *cut;
                self.texts.push(text);
                self.offset = cut;
                self.pending = None;
            }
            // Failed: the final pass covers this audio instead.
            Some(Err(e)) => {
                tracing::warn!("early transcription failed: {e}");
                self.pending = None;
            }
            None if wait || disconnected => self.pending = None,
            None => {}
        }
    }
}

/// Where to end the next piece: the middle of the last pause of at least 300 ms, or the
/// quietest moment when the piece is getting very long. None means keep waiting.
fn find_cut(audio: &[f32], from: usize) -> Option<usize> {
    const FRAME: usize = 320; // 20 ms
    let end = audio.len().saturating_sub(16_000 / 2);
    if end <= from + PIECE_MIN {
        return None;
    }
    let rms = |i: usize| {
        let f = &audio[i..i + FRAME];
        (f.iter().map(|v| v * v).sum::<f32>() / FRAME as f32).sqrt()
    };
    let start = from + PIECE_MIN;
    let mut quiet_run = 0;
    let mut i = end - FRAME;
    while i >= start {
        if rms(i) < 0.01 {
            quiet_run += 1;
            if quiet_run >= 15 {
                return Some(i + quiet_run * FRAME / 2);
            }
        } else {
            quiet_run = 0;
        }
        if i < FRAME {
            break;
        }
        i -= FRAME;
    }
    if end - from < PIECE_FORCE {
        return None;
    }
    (start..end - FRAME)
        .step_by(FRAME)
        .min_by(|a, b| rms(*a).total_cmp(&rms(*b)))
        .map(|i| i + FRAME / 2)
}

const MAX_RECORDING: Duration = Duration::from_secs(5 * 60);

pub enum Input {
    Hotkey(HotkeyEvent),
    Cmd(Command),
    PasteLast,
    /// Start or finish a command-mode dictation (`vaani command`).
    CommandToggle,
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
    /// The current dictation is a command for the selected text.
    command_mode: bool,
    job_cancel: Option<Arc<AtomicBool>>,
    /// When the speaker was last heard, for the hands-free silence stop.
    last_voice: Option<Instant>,
    early: Early,
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
            command_mode: false,
            job_cancel: None,
            last_voice: None,
            early: Early::default(),
            tx,
        };
        // The portal hotkey can wait on a desktop dialog, so start loading the model first.
        c.load_engine();
        c.start_hotkey();
        c
    }

    fn start_hotkey(&mut self) {
        use vaani_core::config::HotkeyBackend as B;
        let backend = match self.config.hotkey.backend {
            B::Auto => None,
            B::X11 => Some(vaani_hotkey::Backend::X11),
            B::Evdev => Some(vaani_hotkey::Backend::Evdev),
            B::Portal => Some(vaani_hotkey::Backend::Portal),
            B::Cli => {
                tracing::info!("hotkey: only `vaani toggle` (set in config)");
                return;
            }
        };
        let (htx, hrx) = crossbeam_channel::unbounded();
        let hk = &self.config.hotkey;
        match vaani_hotkey::listen(&hk.key, &hk.command_key, backend, htx) {
            Ok((l, backend)) => {
                tracing::info!(backend = backend.label(), "hotkey listening");
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
        self.check_silence();
        self.transcribe_early();
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

    /// Let a cloud engine open its connection while the user speaks.
    fn warm_engine(&self) {
        if !matches!(self.shared.engine(), EngineStatus::Ready(_)) {
            return;
        }
        let engine = self.engine.clone();
        let _ = thread::Builder::new().name("warm".into()).spawn(move || {
            // try_lock: never hold up a transcription that is already running.
            if let Ok(mut slot) = engine.try_lock() {
                if let Some(e) = slot.as_mut().filter(|e| e.is_cloud()) {
                    e.warm();
                }
            }
        });
    }

    /// While a long dictation is still being spoken, transcribe what is done so far.
    fn transcribe_early(&mut self) {
        let Some(rec) = &self.recording else { return };
        self.early.collect(false);
        if self.early.pending.is_some()
            || rec.elapsed() < Duration::from_secs(16)
            || self
                .early
                .last_check
                .is_some_and(|t| t.elapsed() < Duration::from_secs(2))
            || !matches!(self.shared.engine(), EngineStatus::Ready(_))
        {
            return;
        }
        self.early.last_check = Some(Instant::now());
        let Ok(audio) = rec.peek() else { return };
        let Some(cut) = find_cut(&audio, self.early.offset) else {
            return;
        };
        let piece = audio[self.early.offset..cut].to_vec();
        let (tx, rx) = crossbeam_channel::bounded(1);
        let engine = self.engine.clone();
        let language = self.config.general.language.clone();
        let _ = thread::Builder::new().name("early".into()).spawn(move || {
            let r = match engine.lock() {
                Ok(mut slot) => match slot.as_mut() {
                    Some(e) => e
                        .transcribe(&piece, &language)
                        .map(|t| t.text)
                        .map_err(|e| e.to_string()),
                    None => Err("no engine".into()),
                },
                Err(_) => Err("engine lock".into()),
            };
            let _ = tx.send(r);
        });
        tracing::debug!(from = self.early.offset, cut, "transcribing a piece early");
        self.early.pending = Some((cut, rx));
    }

    /// Hands-free only: finish once the speaker has been quiet for the configured time.
    fn check_silence(&mut self) {
        let secs = self.config.audio.silence_stop_secs;
        if secs == 0 || self.recording.is_none() || !self.latched {
            return;
        }
        let rms = self.shared.level.get();
        let db = 20.0 * rms.max(1e-6).log10();
        let now = Instant::now();
        // About -42 dBFS: above room noise, below quiet speech.
        if db > -42.0 {
            self.last_voice = Some(now);
            return;
        }
        if let Some(t) = self.last_voice {
            if now.duration_since(t) >= Duration::from_secs(u64::from(secs)) {
                tracing::info!(secs, "silence; finishing hands-free dictation");
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
            Input::CommandToggle => {
                if self.machine.state() == State::Idle {
                    self.command_mode = true;
                }
                self.command(Command::Toggle);
            }
            Input::ReloadEngine => self.load_engine(),
            Input::Config(new) => self.apply_config(*new),
        }
    }

    fn apply_config(&mut self, new: Config) {
        let key_changed = new.hotkey.key != self.config.hotkey.key
            || new.hotkey.command_key != self.config.hotkey.command_key;
        let engine_changed = new.engine != self.config.engine;
        self.config = new;
        if let Ok(mut c) = self.shared.config.lock() {
            *c = self.config.clone();
        }
        if key_changed {
            match &self.hotkey {
                Some(l) => {
                    let hk = &self.config.hotkey;
                    if let Err(e) = l
                        .set_key(&hk.key)
                        .and_then(|()| l.set_command_key(&hk.command_key))
                    {
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
        // The command key works like the dictation key, and marks the dictation as a command.
        let ev = match ev {
            HotkeyEvent::CommandDown => {
                if state == State::Idle {
                    self.command_mode = true;
                }
                HotkeyEvent::Down
            }
            HotkeyEvent::CommandUp => HotkeyEvent::Up,
            ev => ev,
        };
        match (self.config.hotkey.mode, ev) {
            (_, HotkeyEvent::CommandDown | HotkeyEvent::CommandUp) => {}
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
                    self.last_voice = None;
                    self.warm_engine();
                    if self.config.audio.pause_media {
                        crate::media::pause();
                    }
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
                crate::media::resume();
                crate::sounds::play(&self.config, crate::sounds::Cue::Stop);
                match self.recording.take().map(|r| r.finish()) {
                    Some(Ok(samples)) => {
                        let early = std::mem::take(&mut self.early);
                        self.spawn_job(samples, early)
                    }
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
                crate::media::resume();
                crate::sounds::play(&self.config, crate::sounds::Cue::Cancel);
                if let Some(r) = self.recording.take() {
                    r.discard();
                }
                self.early = Early::default();
                self.command_mode = false;
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

    fn spawn_job(&mut self, samples: Vec<f32>, early: Early) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.job_cancel = Some(cancel.clone());
        let ctx = Job {
            engine: self.engine.clone(),
            injector: self.injector.clone(),
            history: self.history.clone(),
            config: self.config.clone(),
            shared: self.shared.clone(),
            cancel,
            early,
            command: std::mem::take(&mut self.command_mode),
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
    early: Early,
    command: bool,
}

impl Job {
    /// Delete the last dictation with BackSpace, if it was inserted in the last 10 minutes.
    fn scratch_last(&self) {
        let last = self
            .shared
            .last_text
            .lock()
            .ok()
            .and_then(|mut l| l.take())
            .filter(|(at, _)| at.elapsed() < Duration::from_secs(600));
        let (Some((_, text)), Some(injector)) = (last, &self.injector) else {
            crate::notify::send(
                "Nothing to scratch",
                "There's no recent dictation to remove.",
            );
            return;
        };
        let n = text.chars().count();
        match injector.backspace(n) {
            Ok(()) => tracing::info!(chars = n, "scratched the last dictation"),
            Err(e) => tracing::warn!("scratch that failed: {e}"),
        }
    }

    /// Command mode: rewrite the selected text as the spoken instruction says, and paste the
    /// result over the selection.
    fn run_command(&self, instruction: &str, app: &str) {
        let Some(injector) = &self.injector else {
            return;
        };
        let Some((provider, key)) = self.llm() else {
            crate::notify::send(
                "Command mode needs AI",
                "Pick an AI provider and add its key in Settings, under AI clean-up.",
            );
            return;
        };
        let selection = injector.selection_text();
        tracing::info!(%app, selection_chars = selection.chars().count(), "command mode");
        let c = &self.config.cleanup;
        let dictionary: Vec<String> = self
            .config
            .dictionary
            .replacements
            .iter()
            .map(|r| r.to.clone())
            .collect();
        let req = vaani_llm::Request {
            provider,
            base_url: &c.ai_base_url,
            model: &c.ai_model,
            api_key: &key,
            prompt: "",
            style: vaani_llm::Style::from_id(&c.style),
            extra: "",
            dictionary: &dictionary,
            // Rewriting takes longer than a clean-up.
            timeout: Duration::from_millis(c.ai_timeout_ms.max(1000) * 4),
        };
        match vaani_llm::command(&selection, instruction, &req) {
            Ok(out) if !self.cancel.load(Ordering::Relaxed) => match injector.insert(&out) {
                Ok(outcome) => tracing::info!(?outcome, "command result inserted"),
                Err(e) => tracing::warn!("command result not inserted: {e}"),
            },
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("command mode: {e}");
                crate::notify::send("Command didn't run", &e.to_string());
            }
        }
    }

    /// The configured AI provider and its key, when it may be used.
    fn llm(&self) -> Option<(&'static vaani_llm::Provider, String)> {
        let c = &self.config.cleanup;
        let provider = vaani_llm::provider(&c.ai_provider)?;
        if self.config.privacy.local_only && !provider.local {
            return None;
        }
        if provider.key_id.is_empty() {
            return Some((provider, String::new()));
        }
        vaani_store::secrets::get_key(provider.key_id).map(|k| (provider, k))
    }

    /// The optional AI pass. None keeps the rule-cleaned text (off, skipped, failed or
    /// rejected by the guardrails).
    fn ai_cleanup(&self, text: &str, app: &str) -> Option<String> {
        let c = &self.config.cleanup;
        if !c.ai_enabled {
            return None;
        }
        let style = match self.config.rule_for(app).map(|r| r.style.as_str()) {
            Some("off") => return None,
            Some(s) if !s.is_empty() => s,
            _ => c.style.as_str(),
        };
        // Terminals and code editors get exactly what was said.
        if vaani_inject::is_terminal(app) || matches!(app, "code" | "code-oss" | "vscodium") {
            return None;
        }
        let provider = vaani_llm::provider(&c.ai_provider)?;
        if self.config.privacy.local_only && !provider.local {
            return None;
        }
        let key = if provider.key_id.is_empty() {
            String::new()
        } else {
            match vaani_store::secrets::get_key(provider.key_id) {
                Some(k) => k,
                None => {
                    tracing::warn!(provider = provider.id, "AI clean-up has no API key");
                    return None;
                }
            }
        };
        let dictionary: Vec<String> = self
            .config
            .dictionary
            .replacements
            .iter()
            .map(|r| r.to.clone())
            .collect();
        let trailing = text.len() - text.trim_end_matches(' ').len();
        let req = vaani_llm::Request {
            provider,
            base_url: &c.ai_base_url,
            model: &c.ai_model,
            api_key: &key,
            prompt: &c.ai_prompt,
            style: vaani_llm::Style::from_id(style),
            extra: &c.ai_instruction,
            dictionary: &dictionary,
            timeout: Duration::from_millis(c.ai_timeout_ms.clamp(300, 10_000)),
        };
        let t = Instant::now();
        match vaani_llm::clean(text, &req) {
            Ok(out) => {
                tracing::info!(
                    took_ms = t.elapsed().as_millis() as u64,
                    "AI clean-up applied"
                );
                Some(format!("{out}{}", " ".repeat(trailing)))
            }
            Err(e) => {
                tracing::info!(
                    took_ms = t.elapsed().as_millis() as u64,
                    "AI clean-up skipped: {e}"
                );
                None
            }
        }
    }

    /// `{date}`, `{time}` and `{clipboard}` in snippets.
    fn fill_variables(&self, text: &str) -> String {
        let now = chrono::Local::now();
        let mut out = text
            .replace("{date}", &now.format("%-d %B %Y").to_string())
            .replace("{time}", &now.format("%H:%M").to_string());
        if out.contains("{clipboard}") {
            let clip = self
                .injector
                .as_ref()
                .map(|i| i.clipboard_text())
                .unwrap_or_default();
            out = out.replace("{clipboard}", &clip);
        }
        out
    }

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

    fn run(mut self, samples: Vec<f32>) {
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
        // Pieces already transcribed while recording; only the rest is left to do.
        self.early.collect(true);
        let done = std::mem::take(&mut self.early.texts);
        let offset = if done.is_empty() {
            0
        } else {
            self.early.offset.min(samples.len())
        };
        let rest = &samples[offset..];
        if !done.is_empty() {
            tracing::debug!(
                pieces = done.len(),
                rest_ms = rest.len() / 16,
                "using early pieces"
            );
        }
        let join = |tail: String| {
            let mut parts = done.clone();
            if !tail.trim().is_empty() {
                parts.push(tail);
            }
            parts.join(" ")
        };
        if rest.len() < MIN_AUDIO_SAMPLES && !done.is_empty() {
            let raw = join(String::new());
            let name = self
                .engine
                .lock()
                .ok()
                .and_then(|s| s.as_ref().map(|e| e.name()))
                .unwrap_or_default();
            return self.finish(raw, name, audio_ms, released);
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
                .transcribe(rest, &self.config.general.language)
                .map(|t| (t, engine.name()))
        };
        let (raw, engine_name) = match result {
            Ok((t, name)) => {
                tracing::info!(audio_ms, took_ms = t.took.as_millis() as u64, "transcribed");
                (join(t.text), name)
            }
            Err(e) => match self.local_fallback(rest) {
                // The cloud failed (offline, quota, outage): use the local model if one is
                // installed, so the words are not lost.
                Some((t, name)) => (join(t), name),
                None => {
                    tracing::error!("transcription failed: {e}");
                    crate::notify::send("Transcription failed", &e.to_string());
                    return;
                }
            },
        };
        self.finish(raw, engine_name, audio_ms, released);
    }

    /// Everything after transcription: commands, clean-up, history and typing.
    fn finish(&self, raw: String, engine_name: String, audio_ms: i64, released: Instant) {
        // "Scratch that": remove the previous dictation instead of typing anything.
        if self.config.dictionary.spoken_commands && vaani_text::is_scratch(&raw) {
            self.scratch_last();
            return;
        }
        let app = self
            .injector
            .as_ref()
            .map(|i| i.focused_app())
            .unwrap_or_default();
        let rule = self.config.rule_for(&app);
        let rules = vaani_text::Rules {
            remove_fillers: self.config.cleanup.remove_fillers,
            trailing_space: rule
                .and_then(|r| r.trailing_space)
                .unwrap_or(self.config.insert.trailing_space),
            spoken_commands: self.config.dictionary.spoken_commands,
            replacements: self
                .config
                .dictionary
                .replacements
                .iter()
                .map(|r| (r.from.clone(), r.to.clone()))
                .collect(),
            snippets: self
                .config
                .dictionary
                .snippets
                .iter()
                .map(|s| (s.trigger.clone(), s.text.clone()))
                .collect(),
        };
        let Some(mut text) = vaani_text::clean(&raw, &rules) else {
            tracing::debug!("nothing to insert");
            return;
        };
        if self.command {
            self.run_command(&text, &app);
            return;
        }
        let snippet = rules
            .snippets
            .iter()
            .any(|(_, body)| text.trim_end() == body.trim_end());
        if !snippet {
            if let Some(polished) = self.ai_cleanup(&text, &app) {
                text = polished;
            }
        }
        if self.config.general.hindi_script == "latin" {
            text = vaani_text::hindi::romanize(&text);
        }
        if text.contains('{') {
            text = self.fill_variables(&text);
        }

        // Never lose words: save before typing.
        if let Some(h) = self.history.as_ref().and_then(|h| h.lock().ok()) {
            let _ = h.count(&text, audio_ms);
        }
        let limit = self.config.privacy.history_limit();
        let id = if !self.config.privacy.saves_history() {
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
        let method = rule
            .and_then(|r| r.insert)
            .unwrap_or(self.config.insert.method);
        let mode = match method {
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
