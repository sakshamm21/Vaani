//! Short synthesized sounds for start/stop feedback, played on a background thread.

use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// (frequency Hz, duration ms) segments, gently faded in and out.
pub fn play_tones(tones: &'static [(f32, u32)], volume: f32) {
    let _ = std::thread::Builder::new()
        .name("cue".into())
        .spawn(move || {
            let host = cpal::default_host();
            let Some(dev) = host.default_output_device() else {
                return;
            };
            let Ok(cfg) = dev.default_output_config() else {
                return;
            };
            let rate = cfg.sample_rate() as f32;
            let channels = usize::from(cfg.channels()).max(1);
            let mut wave: Vec<f32> = Vec::new();
            for &(freq, ms) in tones {
                let n = (rate * ms as f32 / 1000.0) as usize;
                let fade = (rate * 0.008) as usize;
                for i in 0..n {
                    let env = (i.min(n - i) as f32 / fade.max(1) as f32).min(1.0);
                    let t = i as f32 / rate;
                    wave.push((t * freq * std::f32::consts::TAU).sin() * volume * env);
                }
            }
            let total = wave.len();
            let mut pos = 0usize;
            let sc: cpal::StreamConfig = cfg.into();
            let stream = match cfg.sample_format() {
                cpal::SampleFormat::F32 => dev.build_output_stream(
                    sc,
                    move |out: &mut [f32], _| {
                        for frame in out.chunks_mut(channels) {
                            let s = wave.get(pos).copied().unwrap_or(0.0);
                            pos += 1;
                            frame.iter_mut().for_each(|o| *o = s);
                        }
                    },
                    |_| {},
                    None,
                ),
                cpal::SampleFormat::I16 => dev.build_output_stream(
                    sc,
                    move |out: &mut [i16], _| {
                        for frame in out.chunks_mut(channels) {
                            let s = wave.get(pos).copied().unwrap_or(0.0);
                            pos += 1;
                            frame.iter_mut().for_each(|o| *o = (s * 32767.0) as i16);
                        }
                    },
                    |_| {},
                    None,
                ),
                _ => return,
            };
            if let Ok(stream) = stream {
                if stream.play().is_ok() {
                    std::thread::sleep(Duration::from_millis(
                        (total as f32 / rate * 1000.0) as u64 + 120,
                    ));
                }
            }
        });
}
