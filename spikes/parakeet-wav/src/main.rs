//! Spike: transcribe a WAV with NVIDIA Parakeet TDT 0.6B v3 (int8) through sherpa-onnx, on CPU.
//!
//! Questions: does sherpa-onnx build and link cleanly on Mint? How long does the model take to
//! load, how much RAM does it use, and how fast is decoding (real-time factor) on this laptop?
//! The plan's target is under 700 ms from key release to text for a 10-second clip.
//!
//! Setup:  ./fetch-model.sh            (downloads about 640 MB into ./models)
//! Run:    cargo run --release -- WAV_FILE [MODEL_DIR] [THREADS]
//!         e.g. the capture-16k.wav from the mic-capture spike.

use std::time::Instant;

use anyhow::{Context, Result, bail};
use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig, Wave};

const DEFAULT_MODEL: &str = "models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8";

fn rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|kb| kb.parse::<f64>().ok())
        })
        .map(|kb| kb / 1024.0)
        .unwrap_or(0.0)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(wav) = args.first() else {
        bail!("usage: spike-parakeet-wav WAV_FILE [MODEL_DIR] [THREADS]");
    };
    let dir = args.get(1).map(String::as_str).unwrap_or(DEFAULT_MODEL);
    let threads: i32 = args.get(2).and_then(|t| t.parse().ok()).unwrap_or(4);
    for f in [
        "encoder.int8.onnx",
        "decoder.int8.onnx",
        "joiner.int8.onnx",
        "tokens.txt",
    ] {
        if !std::path::Path::new(dir).join(f).exists() {
            bail!("{dir}/{f} missing; run ./fetch-model.sh first");
        }
    }

    let wave = Wave::read(wav).with_context(|| format!("cannot read {wav}"))?;
    let audio_s = wave.num_samples() as f64 / f64::from(wave.sample_rate());
    println!("Audio: {wav}, {} Hz, {audio_s:.2} s", wave.sample_rate());
    println!("RSS before load: {:.0} MB", rss_mb());

    let mut config = OfflineRecognizerConfig::default();
    config.model_config.transducer = OfflineTransducerModelConfig {
        encoder: Some(format!("{dir}/encoder.int8.onnx")),
        decoder: Some(format!("{dir}/decoder.int8.onnx")),
        joiner: Some(format!("{dir}/joiner.int8.onnx")),
    };
    config.model_config.tokens = Some(format!("{dir}/tokens.txt"));
    config.model_config.model_type = Some("nemo_transducer".into());
    config.model_config.num_threads = threads;

    let t = Instant::now();
    let recognizer = OfflineRecognizer::create(&config).context("could not create recognizer")?;
    println!(
        "Model load: {} ms, RSS after load: {:.0} MB",
        t.elapsed().as_millis(),
        rss_mb()
    );

    // Decode twice: the first run includes one-time warm-up, the second is what a user feels.
    for run in 1..=2 {
        let t = Instant::now();
        let stream = recognizer.create_stream();
        stream.accept_waveform(wave.sample_rate(), wave.samples());
        recognizer.decode(&stream);
        let result = stream.get_result().context("no result")?;
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "Run {run}: {ms:.0} ms with {threads} threads, real-time factor {:.3}",
            ms / 1000.0 / audio_s
        );
        if run == 2 {
            println!("Text: {}", result.text.trim());
        }
    }
    println!("RSS peak-ish: {:.0} MB", rss_mb());
    println!("RESULT ok");
    Ok(())
}
