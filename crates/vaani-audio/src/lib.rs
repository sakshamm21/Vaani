//! Microphone capture.
//!
//! [`Recorder::start`] opens the mic (about 25 ms on PipeWire) and collects mono samples;
//! [`Recording::finish`] closes it and returns 16 kHz mono audio ready for speech-to-text.
//! The live input level is published through [`Level`] for the overlay's meter.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rubato::audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{Fft, FixedSync, Resampler};

pub mod cue;

pub const TARGET_RATE: u32 = 16_000;

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("no microphone found")]
    NoDevice,
    #[error("microphone `{0}` not found")]
    DeviceNotFound(String),
    #[error("cannot open the microphone: {0}")]
    Open(String),
    #[error("resampling failed: {0}")]
    Resample(String),
}

/// The most recent input level (RMS of the last callback, 0.0 to 1.0), shared lock-free.
#[derive(Clone, Default)]
pub struct Level(Arc<AtomicU32>);

impl Level {
    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
    fn set(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed);
    }
}

/// Input devices worth showing to a person: skips ALSA plugin pseudo-devices.
pub fn input_devices() -> Vec<String> {
    let host = cpal::default_host();
    let Ok(devices) = host.input_devices() else {
        return Vec::new();
    };
    let mut names: Vec<String> = devices
        .filter(|d| d.default_input_config().is_ok())
        .map(|d| d.to_string())
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            !(l.contains("discard all samples")
                || l.contains("rate converter")
                || l.contains("plugin")
                || l.contains("jack")
                || l.contains("open sound system")
                || l.starts_with("default alsa output"))
        })
        .collect();
    names.dedup();
    names
}

#[derive(Default)]
pub struct Recorder {
    level: Level,
}

pub struct Recording {
    stream: Option<cpal::Stream>,
    samples: Arc<Mutex<Vec<f32>>>,
    rate: u32,
    started: Instant,
    level: Level,
}

impl Recorder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish the input level into an existing [`Level`] (e.g. one the UI already holds).
    pub fn with_level(level: Level) -> Self {
        Self { level }
    }

    pub fn level(&self) -> Level {
        self.level.clone()
    }

    /// Open the mic and start collecting audio. `device` = None or "" means the system default.
    pub fn start(&self, device: Option<&str>) -> Result<Recording, AudioError> {
        // Testing aid: VAANI_FAKE_MIC=/path/to/16k-or-any.wav replays a file instead of the mic.
        if let Some(path) = std::env::var_os("VAANI_FAKE_MIC") {
            let mut r =
                hound::WavReader::open(&path).map_err(|e| AudioError::Open(e.to_string()))?;
            let spec = r.spec();
            let ch = usize::from(spec.channels).max(1);
            let raw: Vec<f32> = match spec.sample_format {
                hound::SampleFormat::Float => r.samples::<f32>().flatten().collect(),
                hound::SampleFormat::Int => {
                    let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
                    r.samples::<i32>()
                        .flatten()
                        .map(|v| v as f32 / scale)
                        .collect()
                }
            };
            let mono: Vec<f32> = raw
                .chunks(ch)
                .map(|f| f.iter().sum::<f32>() / ch as f32)
                .collect();
            let rms = (mono.iter().map(|v| v * v).sum::<f32>() / mono.len().max(1) as f32).sqrt();
            self.level.set(rms.min(1.0));
            return Ok(Recording {
                stream: None,
                samples: Arc::new(Mutex::new(mono)),
                rate: spec.sample_rate,
                started: Instant::now(),
                level: self.level.clone(),
            });
        }
        let host = cpal::default_host();
        let dev = match device.filter(|d| !d.is_empty()) {
            None => host.default_input_device().ok_or(AudioError::NoDevice)?,
            Some(name) => host
                .input_devices()
                .map_err(|e| AudioError::Open(e.to_string()))?
                .find(|d| d.to_string() == name)
                .ok_or_else(|| AudioError::DeviceNotFound(name.to_string()))?,
        };
        let config = dev
            .default_input_config()
            .map_err(|e| AudioError::Open(e.to_string()))?;
        let rate = config.sample_rate();
        let channels = usize::from(config.channels()).max(1);
        let samples = Arc::new(Mutex::new(Vec::with_capacity(rate as usize * 30)));
        let level = self.level.clone();
        level.set(0.0);

        let sink = {
            let samples = samples.clone();
            let level = level.clone();
            move |mono: &mut dyn Iterator<Item = f32>| {
                if let Ok(mut s) = samples.lock() {
                    let start = s.len();
                    s.extend(mono);
                    let chunk = &s[start..];
                    if !chunk.is_empty() {
                        let rms =
                            (chunk.iter().map(|x| x * x).sum::<f32>() / chunk.len() as f32).sqrt();
                        level.set(rms.min(1.0));
                    }
                }
            }
        };
        let err = |e| tracing::warn!("microphone stream error: {e}");
        let sc: cpal::StreamConfig = config.into();
        let open = |e: cpal::Error| AudioError::Open(e.to_string());
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => dev
                .build_input_stream(
                    sc,
                    move |d: &[f32], _| {
                        sink(
                            &mut d
                                .chunks(channels)
                                .map(|f| f.iter().sum::<f32>() / channels as f32),
                        )
                    },
                    err,
                    None,
                )
                .map_err(open)?,
            cpal::SampleFormat::I16 => dev
                .build_input_stream(
                    sc,
                    move |d: &[i16], _| {
                        sink(&mut d.chunks(channels).map(|f| {
                            f.iter().map(|&s| f32::from(s) / 32768.0).sum::<f32>() / channels as f32
                        }))
                    },
                    err,
                    None,
                )
                .map_err(open)?,
            cpal::SampleFormat::I32 => dev
                .build_input_stream(
                    sc,
                    move |d: &[i32], _| {
                        sink(&mut d.chunks(channels).map(|f| {
                            f.iter().map(|&s| s as f32 / 2_147_483_648.0).sum::<f32>()
                                / channels as f32
                        }))
                    },
                    err,
                    None,
                )
                .map_err(open)?,
            other => {
                return Err(AudioError::Open(format!(
                    "unsupported sample format {other:?}"
                )));
            }
        };
        stream.play().map_err(|e| AudioError::Open(e.to_string()))?;
        tracing::debug!(device = %dev, rate, channels, "microphone open");
        Ok(Recording {
            stream: Some(stream),
            samples,
            rate,
            started: Instant::now(),
            level,
        })
    }
}

impl Recording {
    pub fn elapsed(&self) -> std::time::Duration {
        self.started.elapsed()
    }

    /// The audio so far as 16 kHz mono, without stopping (for transcribing long dictations
    /// in pieces while the speaker is still talking).
    pub fn peek(&self) -> Result<Vec<f32>, AudioError> {
        let mono = self.samples.lock().map(|s| s.clone()).unwrap_or_default();
        resample(&mono, self.rate)
    }

    /// Stop the mic and return 16 kHz mono audio.
    pub fn finish(self) -> Result<Vec<f32>, AudioError> {
        if let Some(s) = &self.stream {
            let _ = s.pause();
        }
        drop(self.stream);
        self.level.set(0.0);
        let mono = self.samples.lock().map(|s| s.clone()).unwrap_or_default();
        resample(&mono, self.rate)
    }

    /// Stop and throw the audio away.
    pub fn discard(self) {
        drop(self.stream);
        self.level.set(0.0);
    }
}

pub fn resample(mono: &[f32], rate: u32) -> Result<Vec<f32>, AudioError> {
    if rate == TARGET_RATE || mono.is_empty() {
        return Ok(mono.to_vec());
    }
    let e = |e: &dyn std::fmt::Display| AudioError::Resample(e.to_string());
    let mut r = Fft::<f32>::new(
        rate as usize,
        TARGET_RATE as usize,
        1024,
        1,
        FixedSync::Input,
    )
    .map_err(|x| e(&x))?;
    let input = vec![mono.to_vec()];
    let adapter = SequentialSliceOfVecs::new(&input, 1, mono.len()).map_err(|x| e(&x))?;
    Ok(r.process_all(&adapter, mono.len(), None)
        .map_err(|x| e(&x))?
        .take_data())
}

/// Write 16 kHz mono audio as a 16-bit WAV (used for cloud uploads and debugging).
pub fn wav_bytes(samples: &[f32]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    if let Ok(mut w) = hound::WavWriter::new(&mut buf, spec) {
        for &s in samples {
            let _ = w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16);
        }
        let _ = w.finalize();
    }
    buf.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_48k_to_16k_keeps_duration() {
        let one_second: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
        let out = resample(&one_second, 48_000).unwrap();
        let ratio = out.len() as f32 / 16_000.0;
        assert!((0.95..1.05).contains(&ratio), "{}", out.len());
    }

    #[test]
    fn wav_has_header() {
        let b = wav_bytes(&[0.0; 160]);
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(b.len(), 44 + 320);
    }
}
