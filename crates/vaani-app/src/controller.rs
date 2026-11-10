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

use crate::preview::{self, Preview};

/// A quick tap shorter than this starts hands-free dictation instead of push-to-talk.
const TAP: Duration = Duration::from_millis(300);
/// Clips shorter than this are treated as accidental presses.
const MIN_AUDIO_SAMPLES: usize = 16_000 * 3 / 10;
/// Less speech than this is a tap of the hotkey or a click, not a word.
const MIN_VOICED_MS: u32 = 120;
/// Below this much speech, a lone "Yeah." or "You" is the model guessing at noise.
const PHANTOM_VOICED_MS: u32 = 240;
/// Added to the AI clean-up instructions for Hinglish.
const HINGLISH_INSTRUCTION: &str = "the speaker mixes Hindi and English (Hinglish). Write Hindi \
words in Latin letters the way people type them in chats (\"kya kar rahe ho\", \"theek hai\", \
\"nahi\"), keep English words in English, never translate, and fix words the recognizer \
misheard from context";

/// The Hinglish language setting.
fn is_hinglish(language: &str) -> bool {
    language.eq_ignore_ascii_case(vaani_stt::HINGLISH)
}
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

/// Whisper reads at most 30 s per call (sherpa-onnx drops the rest), and Moonshine is
/// made for short phrases, so their pieces stay under this.
const PIECE_SHORT: usize = 16_000 * 25;

/// Where to end the next piece: the middle of the last clear pause (600 ms, or 300 ms once
/// the piece is long), or the quietest moment when the piece reaches `force` samples.
/// "Quiet" is measured against the recording's own background level, so a quiet laptop mic
/// is not cut in the middle of a word. None means keep waiting.
fn find_cut(audio: &[f32], from: usize, force: usize) -> Option<usize> {
    const FRAME: usize = 320; // 20 ms
    let end = audio.len().saturating_sub(16_000 / 2);
    let min = PIECE_MIN.min(force * 3 / 5);
    if end <= from + min {
        return None;
    }
    let rms = |i: usize| {
        let f = &audio[i..i + FRAME];
        (f.iter().map(|v| v * v).sum::<f32>() / FRAME as f32).sqrt()
    };
    let start = from + min;
    let levels: Vec<(usize, f32)> = (from..end - FRAME)
        .step_by(FRAME)
        .map(|i| (i, rms(i)))
        .collect();
    let mut sorted: Vec<f32> = levels.iter().map(|(_, r)| *r).collect();
    sorted.sort_by(f32::total_cmp);
    // Quiet means near the background (the quietest 1 % of frames; pauses are rare in fluent
    // speech) and well under the typical level, so a quiet mic's speech never counts.
    let floor = sorted.get(sorted.len() / 100).copied().unwrap_or(0.0);
    let median = sorted.get(sorted.len() / 2).copied().unwrap_or(0.0);
    let quiet = (floor * 3.0).min(median * 0.3).clamp(0.0005, 0.01);
    let long = end - from >= force * 4 / 5;
    for need in [30usize, 15] {
        if need == 15 && !long {
            break;
        }
        let mut run = 0;
        for &(i, r) in levels.iter().rev().take_while(|(i, _)| *i >= start) {
            if r < quiet {
                run += 1;
                if run >= need {
                    return Some(i + run * FRAME / 2);
                }
            } else {
                run = 0;
            }
        }
    }
    if end - from < force {
        return None;
    }
    levels
        .iter()
        .filter(|(i, _)| *i >= start)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i + FRAME / 2)
}

/// Split audio into pieces no longer than `max`, cut at pauses.
pub(crate) fn split_pieces(audio: &[f32], max: usize) -> Vec<&[f32]> {
    let mut out = Vec::new();
    let mut from = 0;
    while audio.len() - from > max {
        // Look for a cut inside the next `max` samples (plus the half second find_cut skips).
        let window = &audio[..(from + max + 8_000).min(audio.len())];
        let cut = find_cut(window, from, max)
            .filter(|c| *c > from && *c <= from + max)
            .unwrap_or(from + max);
        out.push(&audio[from..cut]);
        from = cut;
    }
    out.push(&audio[from..]);
    out
}

/// The longest piece the engine should get at once.
pub(crate) fn piece_limit(config: &Config) -> usize {
    if config.engine.kind == EngineKind::Cloud {
        return PIECE_FORCE;
    }
    match vaani_models::find(&config.engine.local_model).map(|m| m.kind) {
        Some(vaani_models::ModelKind::Whisper | vaani_models::ModelKind::Moonshine) => PIECE_SHORT,
        _ => PIECE_FORCE,
    }
}

const MAX_RECORDING: Duration = Duration::from_secs(5 * 60);
/// After the key is let go, keep listening this long: people release the key while the
/// last syllable is still coming out.
const KEY_UP_TAIL: Duration = Duration::from_millis(150);
/// Another key pressed this soon after the hotkey means the hotkey was part of a shortcut.
const CHORD_WINDOW: Duration = Duration::from_millis(1500);

/// Dictations can overlap (a new one starts while the last is still being transcribed), so
/// each takes a ticket and inserts its text only when every earlier one is done.
#[derive(Default)]
struct InsertOrder {
    state: Mutex<(u64, std::collections::BTreeSet<u64>)>,
    turn: std::sync::Condvar,
}

impl InsertOrder {
    /// Wait until every earlier ticket has finished (at most `limit`).
    fn wait_turn(&self, ticket: u64, limit: Duration) {
        let Ok(guard) = self.state.lock() else { return };
        let _ = self
            .turn
            .wait_timeout_while(guard, limit, |(next, _)| *next < ticket);
    }

    /// This ticket is done, inserted or not.
    fn done(&self, ticket: u64) {
        if let Ok(mut g) = self.state.lock() {
            let (next, finished) = &mut *g;
            finished.insert(ticket);
            while finished.remove(next) {
                *next += 1;
            }
        }
        self.turn.notify_all();
    }
}

/// Reports a dictation job as finished however it ends, a panic included, so the app can
/// never be left stuck in "Transcribing".
struct JobGuard {
    tx: Sender<Input>,
    order: Arc<InsertOrder>,
    ticket: u64,
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        self.order.done(self.ticket);
        let _ = self.tx.send(Input::JobDone);
    }
}

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
    /// Live preview of the words being spoken (empty when there is none).
    pub preview: Mutex<String>,
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
    /// Cancel flags of the dictations still being processed.
    job_cancels: Vec<Arc<AtomicBool>>,
    /// Dictation jobs still running.
    jobs: usize,
    order: Arc<InsertOrder>,
    next_ticket: u64,
    /// When the speaker was last heard, for the hands-free silence stop.
    last_voice: Option<Instant>,
    /// The quietest input level of the current recording, in dBFS.
    noise_floor_db: f32,
    early: Early,
    preview: Preview,
    /// With a cloud engine, the local model that reads the live preview (loaded on first use).
    preview_engine: EngineSlot,
    /// When the last recording ended, to unload `preview_engine` after a while.
    last_dictation: Instant,
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
        preview: Mutex::new(String::new()),
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
            job_cancels: Vec::new(),
            jobs: 0,
            order: Arc::new(InsertOrder::default()),
            next_ticket: 0,
            last_voice: None,
            noise_floor_db: 0.0,
            early: Early::default(),
            preview: Preview::default(),
            preview_engine: Arc::new(Mutex::new(None)),
            last_dictation: Instant::now(),
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
        let language = self.config.general.language.clone();
        self.set_engine(EngineStatus::Loading(model.name.clone()));
        let _ = thread::Builder::new().name("engine-load".into()).spawn(move || {
            let mut engine = LocalEngine::new(model.clone(), threads).with_language(&language);
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
        let ticker = tick(Duration::from_millis(100));
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
        self.free_idle_model();
        self.check_silence();
        self.transcribe_early();
        self.update_preview();
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

    /// Let a cloud engine open its connection while the user speaks, and load a local model
    /// that was freed while idle.
    fn warm_engine(&self) {
        if !matches!(self.shared.engine(), EngineStatus::Ready(_)) {
            return;
        }
        let engine = self.engine.clone();
        let _ = thread::Builder::new().name("warm".into()).spawn(move || {
            // try_lock: never hold up a transcription that is already running.
            if let Ok(mut slot) = engine.try_lock() {
                if let Some(e) = slot.as_mut() {
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
        let Some(cut) = find_cut(&audio, self.early.offset, piece_limit(&self.config)) else {
            return;
        };
        let mut piece = audio[self.early.offset..cut].to_vec();
        // The same quiet-speech gain the final pass gets.
        vaani_audio::normalize(&mut piece);
        let context = self.early.texts.join(" ");
        let vocabulary = dictionary_words(&self.config);
        let (tx, rx) = crossbeam_channel::bounded(1);
        let engine = self.engine.clone();
        let language = self.config.general.language.clone();
        let _ = thread::Builder::new().name("early".into()).spawn(move || {
            let r = match engine.lock() {
                Ok(mut slot) => match slot.as_mut() {
                    Some(e) => {
                        e.set_vocabulary(&vocabulary);
                        e.set_context(&context);
                        e.transcribe(&piece, &language)
                            .map(|t| t.text)
                            .map_err(|e| e.to_string())
                    }
                    None => Err("no engine".into()),
                },
                Err(_) => Err("engine lock".into()),
            };
            let _ = tx.send(r);
        });
        tracing::debug!(from = self.early.offset, cut, "transcribing a piece early");
        self.early.pending = Some((cut, rx));
    }

    /// The local model behind the live preview, if it can do one: the main engine when it is
    /// local, else (cloud) the downloaded local model in its own worker. None turns it off.
    fn preview_slot(&self) -> Option<EngineSlot> {
        if !self.config.general.live_preview || !self.config.general.overlay {
            return None;
        }
        let model = vaani_models::find(&self.config.engine.local_model)
            .filter(|m| m.is_installed() && preview::supported(m.kind))?;
        match self.config.engine.kind {
            EngineKind::Local => Some(self.engine.clone()),
            EngineKind::Cloud => {
                let slot = self.preview_engine.clone();
                // Load it in the background the first time; previews start once it is ready.
                if slot.lock().is_ok_and(|s| s.is_none()) {
                    let threads = vaani_models::default_threads() as u32;
                    let loading = slot.clone();
                    let _ = thread::Builder::new()
                        .name("preview-load".into())
                        .spawn(move || {
                            let Ok(mut s) = loading.lock() else { return };
                            if s.is_some() {
                                return;
                            }
                            let mut e = LocalEngine::new(model, threads);
                            match e.warm_up() {
                                Ok(d) => {
                                    tracing::info!(
                                        ms = d.as_millis() as u64,
                                        "preview model ready"
                                    );
                                    *s = Some(Box::new(e));
                                }
                                Err(err) => tracing::warn!("preview model failed to load: {err}"),
                            }
                        });
                }
                Some(slot)
            }
        }
    }

    /// Stop the local model's worker after a while without dictating (General setting).
    fn free_idle_model(&mut self) {
        let mins = self.config.general.free_model_after_mins;
        if mins == 0
            || self.recording.is_some()
            || self.machine.state() != State::Idle
            || self.last_dictation.elapsed() < Duration::from_secs(u64::from(mins) * 60)
        {
            return;
        }
        // try_lock: a transcription or a load in progress means it is in use.
        if let Ok(mut slot) = self.engine.try_lock() {
            if let Some(e) = slot.as_mut() {
                e.unload();
            }
        }
    }

    /// While recording, read the last few seconds again and show the words above the pill.
    fn update_preview(&mut self) {
        let Some(rec) = &self.recording else {
            // Free the cloud user's preview model when it has not been used for a while.
            if self.last_dictation.elapsed() > preview::IDLE_UNLOAD {
                if let Ok(mut s) = self.preview_engine.try_lock() {
                    if s.take().is_some() {
                        tracing::debug!("preview model unloaded");
                    }
                }
            }
            return;
        };
        if self.preview.collect() {
            tracing::debug!(
                took_ms = self.preview.last_took.as_millis() as u64,
                chars = self.preview.text.len(),
                "preview updated"
            );
            if let Ok(mut p) = self.shared.preview.lock() {
                p.clone_from(&self.preview.text);
            }
        }
        // With a local engine the preview shares its model with the final pass, so leave it
        // idle more of the time: the key may be released at any moment.
        let shares_model = self.config.engine.kind == EngineKind::Local;
        if !self.preview.due(shares_model) || rec.elapsed() < Duration::from_millis(500) {
            return;
        }
        let Some(slot) = self.preview_slot() else {
            return;
        };
        let Ok((audio, cut)) = rec.peek_tail(preview::WINDOW) else {
            return;
        };
        let language = self.config.general.language.clone();
        let shared = self.shared.clone();
        let (tx, rx) = crossbeam_channel::bounded(1);
        let _ = thread::Builder::new()
            .name("preview".into())
            .spawn(move || {
                // try_lock: the final pass and early pieces always come first, and a key released
                // meanwhile means the final pass needs the model now.
                let text = slot.try_lock().ok().and_then(|mut s| {
                    if shared.state() != State::Recording {
                        return None;
                    }
                    s.as_mut()
                        .and_then(|e| e.transcribe(&audio, &language).ok())
                        .map(|t| t.text)
                });
                let _ = tx.send(text);
            });
        self.preview.started(cut, rx);
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
        // Speech is judged against this recording's own background: 10 dB above the
        // quietest level heard so far, kept between -55 and -35 dBFS. A fixed level stopped
        // quiet laptop mics while the speaker was still talking.
        if rms > 1e-5 {
            self.noise_floor_db = self.noise_floor_db.min(db);
        }
        let threshold = (self.noise_floor_db + 10.0).clamp(-55.0, -35.0);
        if db > threshold {
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
                self.jobs = self.jobs.saturating_sub(1);
                self.job_cancels.retain(|c| Arc::strong_count(c) > 1);
                if self.jobs == 0 {
                    self.machine.handle(Command::Finished);
                    self.set_state();
                }
            }
            Input::PasteLast => self.paste_last(),
            Input::CommandToggle => {
                if self.machine.state() != State::Recording {
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
        if new.engine.local_model != self.config.engine.local_model
            || new.engine.kind != EngineKind::Cloud
            || !new.general.live_preview
        {
            // A model still loading finishes into the old slot and is dropped with it.
            self.preview_engine = Arc::new(Mutex::new(None));
        }
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
                if state != State::Recording {
                    self.command_mode = true;
                }
                HotkeyEvent::Down
            }
            HotkeyEvent::CommandUp => HotkeyEvent::Up,
            ev => ev,
        };
        match (self.config.hotkey.mode, ev) {
            (_, HotkeyEvent::CommandDown | HotkeyEvent::CommandUp) => {}
            (HotkeyMode::PushToTalk, HotkeyEvent::Chord) => {
                // Right Ctrl+C and the like: the key was used for a shortcut early in the
                // hold, so drop the recording quietly. Later on it is a slip while talking.
                let early = self.down_at.is_some_and(|t| t.elapsed() < CHORD_WINDOW);
                if state == State::Recording && !self.latched && early {
                    tracing::debug!("hotkey used in a shortcut; recording dropped");
                    self.discard_quietly();
                }
            }
            (HotkeyMode::Toggle, HotkeyEvent::Chord) => {}
            (_, HotkeyEvent::Escape) => {
                if state != State::Idle {
                    self.command(Command::Cancel);
                }
            }
            (HotkeyMode::Toggle, HotkeyEvent::Down) => self.command(Command::Toggle),
            (HotkeyMode::Toggle, HotkeyEvent::Up) => {}
            (HotkeyMode::PushToTalk, HotkeyEvent::Down) => match state {
                State::Idle | State::Processing => {
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
                        // Let the last syllable land before the mic closes.
                        thread::sleep(KEY_UP_TAIL);
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
            Effect::StartCapture => match self.open_mic() {
                Ok(r) => {
                    self.recording = Some(r);
                    self.last_voice = None;
                    self.noise_floor_db = 0.0;
                    self.reset_preview();
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
                        if self.jobs == 0 {
                            self.machine.handle(Command::Finished);
                        }
                    }
                    None => {
                        if self.jobs == 0 {
                            self.machine.handle(Command::Finished);
                        }
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
                self.reset_preview();
                self.command_mode = false;
                tracing::info!("dictation cancelled");
            }
            Effect::AbortProcessing => {
                crate::sounds::play(&self.config, crate::sounds::Cue::Cancel);
                for c in &self.job_cancels {
                    c.store(true, Ordering::Relaxed);
                }
                tracing::info!("dictation cancelled while processing");
            }
        }
        self.set_state();
    }

    /// Open the chosen mic; if it has gone (unplugged USB or Bluetooth), the default one, so
    /// the dictation still happens.
    fn open_mic(&self) -> Result<Recording, vaani_audio::AudioError> {
        let chosen = self.config.audio.device.as_str();
        match self.recorder.start(Some(chosen)) {
            Err(vaani_audio::AudioError::DeviceNotFound(name)) => {
                tracing::warn!(%name, "chosen microphone not found; using the default one");
                let r = self.recorder.start(None);
                if r.is_ok() {
                    crate::notify::send(
                        "Using the default microphone",
                        &format!("{name} isn't connected."),
                    );
                }
                r
            }
            other => other,
        }
    }

    /// Drop the recording without the cancel sound (the key was used for a shortcut).
    fn discard_quietly(&mut self) {
        if self.machine.handle(Command::Cancel) != Effect::DiscardCapture {
            self.set_state();
            return;
        }
        self.clear_recording_flags();
        crate::media::resume();
        if let Some(r) = self.recording.take() {
            r.discard();
        }
        self.early = Early::default();
        self.reset_preview();
        self.command_mode = false;
        self.set_state();
    }

    fn reset_preview(&mut self) {
        self.preview = Preview::default();
        if let Ok(mut p) = self.shared.preview.lock() {
            p.clear();
        }
    }

    fn clear_recording_flags(&mut self) {
        self.last_dictation = Instant::now();
        self.latched = false;
        self.down_at = None;
        self.shared.hands_free.store(false, Ordering::Relaxed);
        if let Ok(mut t) = self.shared.recording_since.lock() {
            *t = None;
        }
    }

    fn spawn_job(&mut self, samples: Vec<f32>, early: Early) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.job_cancels.push(cancel.clone());
        let ticket = self.next_ticket;
        self.next_ticket += 1;
        self.jobs += 1;
        let guard = JobGuard {
            tx: self.tx.clone(),
            order: self.order.clone(),
            ticket,
        };
        let ctx = Job {
            engine: self.engine.clone(),
            injector: self.injector.clone(),
            history: self.history.clone(),
            config: self.config.clone(),
            shared: self.shared.clone(),
            cancel,
            early,
            command: std::mem::take(&mut self.command_mode),
            order: self.order.clone(),
            ticket,
        };
        let spawned = thread::Builder::new()
            .name("dictation".into())
            .spawn(move || {
                // Dropped when the job ends, even by a panic: reports it done.
                let _guard = guard;
                ctx.run(samples);
            });
        if let Err(e) = spawned {
            tracing::error!("cannot start the dictation job: {e}");
        }
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

/// The Dictionary's words, as hints for engines that take them.
pub(crate) fn dictionary_words(config: &Config) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    for r in &config.dictionary.replacements {
        let w = r.to.trim();
        // Short names and terms; whole sentences and addresses are not words to hear.
        if !w.is_empty()
            && w.split_whitespace().count() <= 3
            && !w.contains('\n')
            && !words.iter().any(|x| x == w)
        {
            words.push(w.to_string());
        }
    }
    words
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
    order: Arc<InsertOrder>,
    ticket: u64,
}

impl Job {
    fn vocabulary(&self) -> Vec<String> {
        dictionary_words(&self.config)
    }

    /// What was said just before, for engines that take a prompt: the earlier pieces of this
    /// dictation, else the last dictation if it was in the last three minutes.
    fn context(&self, done: &[String]) -> String {
        if !done.is_empty() {
            return done.join(" ");
        }
        self.shared
            .last_text
            .lock()
            .ok()
            .and_then(|l| l.clone())
            .filter(|(at, _)| at.elapsed() < Duration::from_secs(180))
            .map(|(_, t)| t)
            .unwrap_or_default()
    }

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
        let mut extra = c.ai_instruction.trim().to_string();
        if is_hinglish(&self.config.general.language) {
            if !extra.is_empty() {
                extra.push_str("; ");
            }
            extra.push_str(HINGLISH_INSTRUCTION);
        }
        let req = vaani_llm::Request {
            provider,
            base_url: &c.ai_base_url,
            model: &c.ai_model,
            api_key: &key,
            prompt: &c.ai_prompt,
            style: vaani_llm::Style::from_id(style),
            extra: &extra,
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
        let mut engine =
            LocalEngine::new(model, threads).with_language(&self.config.general.language);
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
        // A tap of the hotkey, or a pause without speaking: models invent words ("Yeah.")
        // from the click and the hum, so nothing is sent to them.
        let voiced = vaani_audio::voiced_ms(&samples);
        if voiced < MIN_VOICED_MS {
            tracing::debug!(audio_ms, voiced_ms = voiced, "no speech; ignored");
            return;
        }
        let mut samples = samples;
        let gain = vaani_audio::normalize(&mut samples);
        if gain > 1.0 {
            tracing::debug!(gain, "quiet recording raised");
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
            return self.finish(raw, name, audio_ms, voiced, released);
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
            engine.set_vocabulary(&self.vocabulary());
            let mut context = self.context(&done);
            let language = &self.config.general.language;
            // Whisper and Moonshine get the audio in pieces they can read whole.
            let mut texts: Vec<String> = Vec::new();
            let mut took = Duration::ZERO;
            let mut result = Ok(());
            for piece in split_pieces(rest, piece_limit(&self.config)) {
                engine.set_context(&context);
                match engine.transcribe(piece, language) {
                    Ok(t) => {
                        took += t.took;
                        if !t.text.trim().is_empty() {
                            context = format!("{context} {}", t.text.trim());
                            texts.push(t.text.trim().to_string());
                        }
                    }
                    Err(e) => {
                        result = Err(e);
                        break;
                    }
                }
            }
            engine.set_context("");
            result.map(|()| {
                (
                    vaani_stt::Transcript {
                        text: texts.join(" "),
                        took,
                    },
                    engine.name(),
                )
            })
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
        self.finish(raw, engine_name, audio_ms, voiced, released);
    }

    /// Everything after transcription: commands, clean-up, history and typing.
    fn finish(
        &self,
        raw: String,
        engine_name: String,
        audio_ms: i64,
        voiced_ms: u32,
        released: Instant,
    ) {
        // "Yeah." or "You" from a breath or a short noise, not from someone speaking.
        if voiced_ms < PHANTOM_VOICED_MS && vaani_text::is_phantom(&raw) {
            tracing::debug!(voiced_ms, "short noise read as a word; ignored");
            return;
        }
        // "Scratch that": remove the previous dictation instead of typing anything.
        if self.config.dictionary.spoken_commands && vaani_text::is_scratch(&raw) {
            self.order.wait_turn(self.ticket, Duration::from_secs(60));
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
            full_stop: match self.config.insert.full_stop {
                vaani_core::config::FullStop::Smart => vaani_text::FullStop::Smart,
                vaani_core::config::FullStop::Always => vaani_text::FullStop::Keep,
                vaani_core::config::FullStop::Never => vaani_text::FullStop::Never,
            },
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
            self.order.wait_turn(self.ticket, Duration::from_secs(60));
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
        if self.config.general.hindi_script == "latin" || is_hinglish(&self.config.general.language)
        {
            text = vaani_text::hindi::romanize(&text);
        }
        // The AI pass and the Latin spelling can bring a full stop back.
        if !snippet {
            text = vaani_text::apply_full_stop(
                &text,
                rules.full_stop,
                rules.spoken_commands && vaani_text::ends_with_spoken_stop(&raw),
            );
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
        // An earlier dictation still being typed goes first.
        self.order.wait_turn(self.ticket, Duration::from_secs(60));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Speech-like noise at `amp` for `secs`, then a pause of `pause_ms` at `floor`.
    fn speech(secs: usize, amp: f32) -> Vec<f32> {
        (0..16_000 * secs)
            .map(|i| amp * ((i as f32 * 0.13).sin() + (i as f32 * 0.029).sin()) / 2.0)
            .collect()
    }
    fn pause(ms: usize, floor: f32) -> Vec<f32> {
        (0..16 * ms)
            .map(|i| floor * (i as f32 * 0.7).sin())
            .collect()
    }

    #[test]
    fn quiet_mic_is_cut_at_its_pause() {
        // A quiet speaker (peaks at 0.008, under the old fixed 0.01 "quiet" level) with
        // one clear pause at 18 s.
        let mut a = speech(18, 0.008);
        a.extend(pause(800, 0.0005));
        a.extend(speech(6, 0.008));
        let cut = find_cut(&a, 0, PIECE_FORCE).expect("a cut");
        let at = cut as f32 / 16_000.0;
        assert!((18.0..18.8).contains(&at), "cut at {at}");
    }

    #[test]
    fn short_breaths_are_not_cuts_until_the_piece_is_long() {
        let mut a = speech(16, 0.2);
        a.extend(pause(400, 0.001));
        a.extend(speech(3, 0.2));
        assert_eq!(find_cut(&a, 0, PIECE_FORCE), None);
    }

    #[test]
    fn whisper_pieces_stay_under_30_seconds() {
        // 70 s with no pause at all.
        let a = speech(70, 0.2);
        let pieces = split_pieces(&a, PIECE_SHORT);
        assert!(pieces.len() >= 3);
        assert!(pieces.iter().all(|p| p.len() <= PIECE_SHORT));
        assert_eq!(pieces.iter().map(|p| p.len()).sum::<usize>(), a.len());
        // Short audio stays whole.
        assert_eq!(split_pieces(&a[..16_000 * 5], PIECE_SHORT).len(), 1);
    }

    #[test]
    fn overlapping_dictations_insert_in_order() {
        let order = Arc::new(InsertOrder::default());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut handles = Vec::new();
        // Ticket 1 is ready first but must wait for ticket 0.
        for (ticket, delay) in [(1u64, 0u64), (0, 80)] {
            let (o, s) = (order.clone(), seen.clone());
            handles.push(thread::spawn(move || {
                thread::sleep(Duration::from_millis(delay));
                o.wait_turn(ticket, Duration::from_secs(5));
                s.lock().unwrap().push(ticket);
                o.done(ticket);
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(*seen.lock().unwrap(), vec![0, 1]);
    }

    #[test]
    fn a_failed_dictation_does_not_block_the_next() {
        let order = InsertOrder::default();
        order.done(0); // finished without inserting
        let t = Instant::now();
        order.wait_turn(1, Duration::from_secs(5));
        assert!(t.elapsed() < Duration::from_secs(1));
    }
}
