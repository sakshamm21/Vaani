//! The speech worker: loads a local model and runs speech-to-text in a supervised child
//! process, talking to the app over stdin/stdout (see `vaani_stt::protocol`). A crash or an
//! out-of-memory kill here never takes down the app.
//!
//! On Linux it runs as the `vaani-worker` program. On Windows the same code is built into
//! `vaani.exe`, which starts a copy of itself in worker mode: Smart App Control and other
//! application control policies judge each unsigned program separately, and one program
//! that is allowed to run is all a user's machine has to accept.
//!
//! Also usable by hand for testing: `vaani-worker --transcribe MODEL_ID FILE.wav`.

use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::time::Instant;

use sherpa_onnx::{
    OfflineMoonshineModelConfig, OfflineRecognizer, OfflineRecognizerConfig,
    OfflineSenseVoiceModelConfig, OfflineTransducerModelConfig, OfflineWhisperModelConfig,
};
use vaani_models::{ModelKind, find_file, find_tokens};
use vaani_stt::protocol::{self, Request, Response};

fn path(p: Option<std::path::PathBuf>, what: &str) -> Result<Option<String>, String> {
    p.map(|p| Some(p.display().to_string()))
        .ok_or_else(|| format!("model file `{what}` not found"))
}

fn load(kind: ModelKind, dir: &Path, threads: u32) -> Result<OfflineRecognizer, String> {
    let mut c = OfflineRecognizerConfig::default();
    c.model_config.num_threads = i32::try_from(threads).unwrap_or(4);
    c.model_config.tokens = path(find_tokens(dir), "tokens.txt")?;
    match kind {
        ModelKind::NemoTransducer => {
            c.model_config.transducer = OfflineTransducerModelConfig {
                encoder: path(find_file(dir, "encoder"), "encoder")?,
                decoder: path(find_file(dir, "decoder"), "decoder")?,
                joiner: path(find_file(dir, "joiner"), "joiner")?,
            };
            c.model_config.model_type = Some("nemo_transducer".into());
        }
        ModelKind::Whisper => {
            c.model_config.whisper = OfflineWhisperModelConfig {
                encoder: path(find_file(dir, "encoder"), "encoder")?,
                decoder: path(find_file(dir, "decoder"), "decoder")?,
                task: Some("transcribe".into()),
                tail_paddings: -1,
                ..Default::default()
            };
        }
        ModelKind::Moonshine => {
            c.model_config.moonshine = OfflineMoonshineModelConfig {
                preprocessor: path(find_file(dir, "preprocess"), "preprocess")?,
                encoder: path(find_file(dir, "encode"), "encode")?,
                uncached_decoder: path(find_file(dir, "uncached_decode"), "uncached_decode")?,
                cached_decoder: path(find_file(dir, "cached_decode"), "cached_decode")?,
                merged_decoder: None,
            };
        }
        ModelKind::SenseVoice => {
            c.model_config.sense_voice = OfflineSenseVoiceModelConfig {
                model: path(find_file(dir, "model"), "model")?,
                language: Some("auto".into()),
                use_itn: true,
            };
        }
    }
    OfflineRecognizer::create(&c).ok_or_else(|| "the model failed to load".to_string())
}

fn transcribe(r: &OfflineRecognizer, samples: &[f32]) -> String {
    let stream = r.create_stream();
    stream.accept_waveform(16_000, samples);
    r.decode(&stream);
    stream.get_result().map(|r| r.text).unwrap_or_default()
}

fn serve() -> i32 {
    let mut rx = BufReader::new(std::io::stdin().lock());
    let mut tx = BufWriter::new(std::io::stdout().lock());
    let mut recognizer: Option<OfflineRecognizer> = None;
    loop {
        let req: Request = match protocol::read(&mut rx) {
            Ok(r) => r,
            Err(_) => return 0, // app went away
        };
        let resp = match req {
            Request::Shutdown => return 0,
            Request::Load { kind, dir, threads } => {
                let t = Instant::now();
                match load(kind, Path::new(&dir), threads) {
                    Ok(r) => {
                        recognizer = Some(r);
                        Response::Loaded {
                            ms: t.elapsed().as_millis() as u64,
                        }
                    }
                    Err(e) => Response::Error(e),
                }
            }
            Request::Transcribe {
                samples,
                language: _,
            } => match &recognizer {
                None => Response::Error("no model loaded".into()),
                Some(r) => {
                    let t = Instant::now();
                    let text = transcribe(r, &samples);
                    Response::Text {
                        text,
                        ms: t.elapsed().as_millis() as u64,
                    }
                }
            },
        };
        if protocol::write(&mut tx, &resp).is_err() {
            return 0;
        }
    }
}

/// Run the worker with its command-line arguments (without the program name) and return the
/// exit code. `--serve` speaks the protocol on stdin and stdout.
pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("--serve") => serve(),
        Some("--version") | Some("-V") => {
            println!("vaani-worker {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Some("--transcribe") if args.len() == 3 => {
            let Some(model) = vaani_models::find(&args[1]) else {
                eprintln!("unknown model {}", args[1]);
                std::process::exit(2);
            };
            let Some(dir) = model.dir().filter(|_| model.is_installed()) else {
                eprintln!(
                    "model {} is not downloaded (run `vaani models download {}`)",
                    model.id, model.id
                );
                std::process::exit(2);
            };
            let Some(wave) = sherpa_onnx::Wave::read(&args[2]) else {
                eprintln!("cannot read {}", args[2]);
                std::process::exit(2);
            };
            let t = Instant::now();
            match load(model.kind, &dir, vaani_models::default_threads() as u32) {
                Ok(r) => {
                    let load_ms = t.elapsed().as_millis();
                    let t = Instant::now();
                    let stream = r.create_stream();
                    stream.accept_waveform(wave.sample_rate(), wave.samples());
                    r.decode(&stream);
                    let text = stream.get_result().map(|r| r.text).unwrap_or_default();
                    println!("{}", text.trim());
                    eprintln!("load {load_ms} ms, decode {} ms", t.elapsed().as_millis());
                    0
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }
        _ => {
            eprintln!(
                "vaani-worker is started by the vaani app; it is not meant to be run by hand."
            );
            2
        }
    }
}
