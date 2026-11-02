//! Spike: capture the microphone with cpal (through PipeWire's ALSA layer), downmix to mono,
//! resample to 16 kHz with rubato and write WAV files.
//!
//! Questions: which device and format do we get by default on Mint? How long from "open the
//! mic" to the first audio callback (this sets how much pre-roll we need)? Does the 16 kHz
//! output sound right?
//!
//! Run:   cargo run --release -- [SECONDS]      (default 5)
//!        Speak during the recording. Writes capture-native.wav and capture-16k.wav here.
//!        `cargo run -- --list` prints input devices.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rubato::audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{Fft, FixedSync, Resampler};

const TARGET_RATE: u32 = 16_000;

fn main() -> Result<()> {
    let arg = std::env::args().nth(1);
    let host = cpal::default_host();
    println!("Audio host: {:?}", host.id());

    if arg.as_deref() == Some("--list") {
        for d in host.input_devices()? {
            let cfg = d
                .default_input_config()
                .map(|c| format!("{c:?}"))
                .unwrap_or_default();
            println!("  {d}  {cfg}");
        }
        return Ok(());
    }
    let seconds: f32 = arg.and_then(|s| s.parse().ok()).unwrap_or(5.0);

    let device = host
        .default_input_device()
        .context("no default input device")?;
    let config = device.default_input_config()?;
    let rate = config.sample_rate();
    let channels = config.channels() as usize;
    println!("Device: {device}");
    println!(
        "Format: {:?}, {rate} Hz, {channels} ch",
        config.sample_format()
    );

    let samples: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let first_cb: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let opened = Instant::now();

    let push = {
        let samples = samples.clone();
        let first_cb = first_cb.clone();
        move |mono: &mut dyn Iterator<Item = f32>| {
            if let Ok(mut f) = first_cb.lock() {
                f.get_or_insert_with(Instant::now);
            }
            if let Ok(mut s) = samples.lock() {
                s.extend(mono);
            }
        }
    };
    let err = |e| eprintln!("stream error: {e}");
    let stream_config: cpal::StreamConfig = config.clone().into();
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => {
            let push = push;
            device.build_input_stream(
                stream_config.clone(),
                move |data: &[f32], _| {
                    push(
                        &mut data
                            .chunks(channels)
                            .map(|f| f.iter().sum::<f32>() / channels as f32),
                    )
                },
                err,
                None,
            )?
        }
        cpal::SampleFormat::I16 => {
            let push = push;
            device.build_input_stream(
                stream_config.clone(),
                move |data: &[i16], _| {
                    push(&mut data.chunks(channels).map(|f| {
                        f.iter().map(|&s| f32::from(s) / 32768.0).sum::<f32>() / channels as f32
                    }))
                },
                err,
                None,
            )?
        }
        other => bail!("sample format {other:?} not handled in this spike"),
    };
    stream.play()?;
    println!("Recording {seconds} s... speak now.");
    std::thread::sleep(Duration::from_secs_f32(seconds));
    drop(stream);

    let mono = samples.lock().map(|s| s.clone()).unwrap_or_default();
    let startup = first_cb
        .lock()
        .ok()
        .and_then(|f| *f)
        .map(|t| t.duration_since(opened).as_millis());
    println!("Mic open to first callback: {startup:?} ms");
    println!(
        "Captured {} frames = {:.2} s",
        mono.len(),
        mono.len() as f32 / rate as f32
    );
    if mono.is_empty() {
        bail!("no audio captured");
    }
    let peak = mono.iter().fold(0f32, |m, s| m.max(s.abs()));
    let rms = (mono.iter().map(|s| s * s).sum::<f32>() / mono.len() as f32).sqrt();
    println!(
        "Peak {:.3} ({:.1} dBFS), RMS {:.4} ({:.1} dBFS)",
        peak,
        20.0 * peak.max(1e-9).log10(),
        rms,
        20.0 * rms.max(1e-9).log10()
    );
    if peak < 0.01 {
        println!("WARNING: nearly silent. Is the right mic selected and unmuted?");
    }

    write_wav("capture-native.wav", rate, &mono)?;

    let t = Instant::now();
    let resampled = if rate == TARGET_RATE {
        mono.clone()
    } else {
        let mut r = Fft::<f32>::new(
            rate as usize,
            TARGET_RATE as usize,
            1024,
            1,
            FixedSync::Input,
        )?;
        let input = vec![mono.clone()];
        let adapter = SequentialSliceOfVecs::new(&input, 1, mono.len())?;
        r.process_all(&adapter, mono.len(), None)?.take_data()
    };
    println!(
        "Resampled {rate} -> {TARGET_RATE} Hz in {} ms ({} frames)",
        t.elapsed().as_millis(),
        resampled.len()
    );
    write_wav("capture-16k.wav", TARGET_RATE, &resampled)?;
    println!("RESULT ok: wrote capture-native.wav and capture-16k.wav");
    Ok(())
}

fn write_wav(path: &str, rate: u32, data: &[f32]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for &s in data {
        w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)?;
    }
    w.finalize()?;
    Ok(())
}
