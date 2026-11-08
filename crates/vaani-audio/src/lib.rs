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

    /// The last `samples_16k` samples so far, at 16 kHz, without copying the whole recording.
    /// The flag says whether earlier audio was left out.
    pub fn peek_tail(&self, samples_16k: usize) -> Result<(Vec<f32>, bool), AudioError> {
        let native = (samples_16k as u64 * u64::from(self.rate) / u64::from(TARGET_RATE)) as usize;
        let (mono, cut) = self
            .samples
            .lock()
            .map(|s| {
                let from = s.len().saturating_sub(native);
                (s[from..].to_vec(), from > 0)
            })
            .unwrap_or_default();
        Ok((resample(&mono, self.rate)?, cut))
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

/// Roughly how many milliseconds of a 16 kHz recording are speech rather than silence,
/// hum or the click of the hotkey. Frames of 20 ms count when they are clearly louder than
/// the recording's own quiet parts; the first and last 100 ms, where key clicks land, are
/// left out of longer recordings.
pub fn voiced_ms(samples: &[f32]) -> u32 {
    const FRAME: usize = 320;
    const EDGE: usize = 1_600;
    let body = if samples.len() > 4 * EDGE + FRAME {
        &samples[EDGE..samples.len() - EDGE]
    } else {
        samples
    };
    let rms: Vec<f32> = body
        .chunks_exact(FRAME)
        .map(|f| (f.iter().map(|v| v * v).sum::<f32>() / FRAME as f32).sqrt())
        .collect();
    if rms.is_empty() {
        return 0;
    }
    let mut sorted = rms.clone();
    sorted.sort_by(f32::total_cmp);
    let floor = sorted[sorted.len() / 10];
    // Three times the background, but never so high that ordinary speech misses it.
    let threshold = (floor * 3.0).clamp(0.004, 0.02);
    let frames = rms.iter().filter(|r| **r > threshold).count();
    (frames * 20) as u32
}

/// Raise quiet speech (a laptop mic across the room, a whisper) to a normal level before it
/// goes to the speech model. The level is set from the loudest 0.1 % of samples, so a single
/// click doesn't count; gain is capped at 10x (20 dB) so background hiss stays quiet, and
/// recordings that are already loud enough are left as they are. Returns the gain used.
pub fn normalize(samples: &mut [f32]) -> f32 {
    const TARGET: f32 = 0.5;
    const LOUD_ENOUGH: f32 = 0.25;
    const MAX_GAIN: f32 = 10.0;
    if samples.is_empty() {
        return 1.0;
    }
    let mut mags: Vec<f32> = samples.iter().map(|v| v.abs()).collect();
    let k = mags.len() - 1 - mags.len() / 1000;
    let (_, peak, _) = mags.select_nth_unstable_by(k, f32::total_cmp);
    let peak = *peak;
    if peak >= LOUD_ENOUGH || peak <= 1e-4 {
        return 1.0;
    }
    let gain = (TARGET / peak).min(MAX_GAIN);
    for v in samples.iter_mut() {
        *v = (*v * gain).clamp(-1.0, 1.0);
    }
    gain
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

    #[test]
    fn normalize_raises_quiet_speech_only() {
        let tone =
            |amp: f32| -> Vec<f32> { (0..16_000).map(|i| amp * (i as f32 * 0.05).sin()).collect() };
        let mut quiet = tone(0.05);
        let g = super::normalize(&mut quiet);
        assert!((g - 10.0).abs() < 0.01, "{g}");
        let mut soft = tone(0.1);
        let g = super::normalize(&mut soft);
        assert!((g - 5.0).abs() < 0.1, "{g}");
        let peak = soft.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((peak - 0.5).abs() < 0.02, "{peak}");
        let mut loud = tone(0.6);
        assert_eq!(super::normalize(&mut loud), 1.0);
        let mut silent = vec![0.0; 1000];
        assert_eq!(super::normalize(&mut silent), 1.0);
        // One loud click doesn't stop quiet speech from being raised.
        let mut clicked = tone(0.05);
        clicked[100] = 0.9;
        assert!(super::normalize(&mut clicked) > 9.0);
    }

    use super::*;

    fn tone(ms: usize, amp: f32) -> Vec<f32> {
        (0..ms * 16).map(|i| amp * (i as f32 * 0.1).sin()).collect()
    }

    #[test]
    fn voiced_time() {
        // Silence with a key click at each end.
        let mut tap = tone(30, 0.3);
        tap.extend(tone(400, 0.001));
        tap.extend(tone(30, 0.3));
        assert!(voiced_ms(&tap) < 100, "{}", voiced_ms(&tap));
        assert_eq!(voiced_ms(&[]), 0);
        // A word between two pauses.
        let mut word = tone(300, 0.002);
        word.extend(tone(400, 0.1));
        word.extend(tone(300, 0.002));
        let v = voiced_ms(&word);
        assert!((300..=420).contains(&v), "{v}");
    }

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
