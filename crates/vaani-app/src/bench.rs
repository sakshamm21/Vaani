//! `vaani bench`: measure how well and how fast a speech engine hears you.
//!
//! A test set is a folder of WAV files, each with a `.txt` file of the same name holding
//! what was said. `vaani bench DIR` transcribes every clip the way the app does (same gain,
//! same pieces, same context between pieces) and prints the word error rate and the time
//! taken. `vaani bench DIR --record` builds such a folder from your own voice.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use vaani_core::config::{Config, EngineKind};
use vaani_stt::SttEngine;

/// What the guided recorder asks you to read: everyday English, Hinglish, names, numbers,
/// a quiet sentence and a long one.
const SENTENCES: &[&str] = &[
    "Can you send me the report by Friday evening?",
    "I'll be ten minutes late, the traffic near the metro station is terrible.",
    "Let's schedule the meeting for half past three tomorrow.",
    "Please add milk, bread, eggs and two kilos of rice to the list.",
    "The total comes to four thousand two hundred and fifty rupees.",
    "My flight leaves at six forty five in the morning from terminal three.",
    "Thanks for the update, I'll review the pull request tonight.",
    "Could you share the slides from yesterday's presentation?",
    "Haan yaar, main kal office aa raha hoon, meeting gyarah baje hai.",
    "Kya scene hai, sab theek? Call me when you're free.",
    "Mujhe lagta hai ki yeh feature next week tak ready ho jayega.",
    "Bhai, please report ready rakhna, client ko aaj hi bhejna hai.",
    "Main abhi traffic mein hoon, thodi der mein pahunchta hoon.",
    "Theek hai, done, I'll send it after lunch.",
    "Kal ki meeting cancel ho gayi, ab Monday ko karenge.",
    "Order confirm ho gaya, delivery Saturday tak aa jayegi.",
    "Vaani is a voice dictation app for Linux and Windows.",
    "Open GitHub and check the latest release of Vaani.",
    "Send an email to Saksham about the Microsoft Store listing.",
    "The meeting with Priya and Rohan moved to Wednesday.",
    "Book a table for four at eight thirty tonight.",
    "My phone number ends with nine eight seven six.",
    "Remind me to call mom at seven.",
    "The temperature in Delhi will be around twenty eight degrees.",
    "Please speak quietly: this sentence is read softly, close to a whisper.",
    "I think we should ship the update first and fix the smaller issues later.",
    "Thoda sa change karna hai, button ka colour pink kar do.",
    "Dinner ke liye kya order karein, pizza ya biryani?",
    "Let me know if you need anything else from my side.",
    "Today I want to talk about how we plan the next release. First we measure how good \
     dictation is today, then we fix the problems that lose words, and after that we work \
     on accuracy, speed and the small details that make the app feel finished. Each step \
     ships on its own so we can test it properly before moving on.",
];

pub fn run(dir: PathBuf, record: bool, model: Option<String>, cloud: bool) -> Result<ExitCode> {
    crate::logging::init_cli();
    if record {
        return record_set(&dir);
    }
    let config = vaani_core::paths::config_file()
        .and_then(|p| Config::load(&p).ok())
        .unwrap_or_default();
    let clips = clips(&dir)?;
    if clips.is_empty() {
        bail!(
            "no clips in {} (each WAV needs a .txt with the same name; `vaani bench {} --record` makes them)",
            dir.display(),
            dir.display()
        );
    }
    let (mut engine, config) = engine(config, model, cloud)?;
    println!("Engine: {}", engine.name());
    println!(
        "{:<34} {:>6} {:>8} {:>7}",
        "CLIP", "AUDIO", "TIME", "ERRORS"
    );
    let (mut errors, mut words) = (0usize, 0usize);
    let (mut audio_total, mut took_total) = (Duration::ZERO, Duration::ZERO);
    for (wav, reference) in clips {
        let mut samples = vaani_audio::read_wav_16k(&wav)
            .with_context(|| format!("reading {}", wav.display()))?;
        let audio = Duration::from_millis(samples.len() as u64 * 1000 / 16_000);
        vaani_audio::normalize(&mut samples);
        let t = Instant::now();
        let text = transcribe(engine.as_mut(), &samples, &config)?;
        let took = t.elapsed();
        let text = if config.general.hindi_script == "latin"
            || config
                .general
                .language
                .eq_ignore_ascii_case(vaani_stt::HINGLISH)
        {
            vaani_text::hindi::romanize(&text)
        } else {
            text
        };
        let (e, n) = word_errors(&reference, &text);
        errors += e;
        words += n;
        audio_total += audio;
        took_total += took;
        let name = wav
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        println!(
            "{:<34} {:>5.1}s {:>6}ms {:>6.1}%",
            name,
            audio.as_secs_f32(),
            took.as_millis(),
            percent(e, n)
        );
        if e > 0 {
            println!("    said:  {}", reference.trim());
            println!("    heard: {}", text.trim());
        }
    }
    println!();
    println!(
        "Word error rate {:.1}% over {} words; {:.1}s of audio in {:.1}s",
        percent(errors, words),
        words,
        audio_total.as_secs_f32(),
        took_total.as_secs_f32()
    );
    Ok(ExitCode::SUCCESS)
}

fn percent(errors: usize, words: usize) -> f64 {
    if words == 0 {
        0.0
    } else {
        errors as f64 * 100.0 / words as f64
    }
}

/// The WAV files in `dir` that have a reference text, sorted by name.
fn clips(dir: &Path) -> Result<Vec<(PathBuf, String)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("wav") {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(path.with_extension("txt")) {
            out.push((path, text));
        }
    }
    out.sort();
    Ok(out)
}

/// The engine to measure: `--model` picks a downloaded local model, `--cloud` the cloud
/// provider from Settings; otherwise whatever Settings use.
fn engine(
    mut config: Config,
    model: Option<String>,
    cloud: bool,
) -> Result<(Box<dyn SttEngine>, Config)> {
    if let Some(id) = model {
        config.engine.kind = EngineKind::Local;
        config.engine.local_model = id;
    } else if cloud {
        config.engine.kind = EngineKind::Cloud;
    }
    let e = &config.engine;
    let engine: Box<dyn SttEngine> = if e.kind == EngineKind::Cloud {
        let Some(provider) = vaani_stt::cloud::provider(&e.cloud_provider) else {
            bail!("unknown cloud provider {}", e.cloud_provider);
        };
        let key = vaani_store::secrets::get_key(provider.id).unwrap_or_default();
        if key.is_empty() && provider.id != "openai_compatible" {
            bail!(
                "no {} API key; add it in Settings, under Cloud",
                provider.name
            );
        }
        Box::new(vaani_stt::cloud::CloudEngine::new(
            provider,
            &e.cloud_base_url,
            &e.cloud_model,
            &key,
        )?)
    } else {
        let Some(m) = vaani_models::find(&e.local_model) else {
            bail!("unknown model {}", e.local_model);
        };
        if !m.is_installed() {
            bail!(
                "model {} is not downloaded (vaani models download {})",
                m.id,
                m.id
            );
        }
        let threads = match e.threads {
            0 => vaani_models::default_threads() as u32,
            n => n,
        };
        let mut local =
            vaani_stt::local::LocalEngine::new(m, threads).with_language(&config.general.language);
        let load = local.warm_up()?;
        println!("Model loaded in {} ms", load.as_millis());
        Box::new(local)
    };
    Ok((engine, config))
}

/// Transcribe a clip as the app does: Dictionary hints, pieces the engine can read whole,
/// and each piece told what came before it.
fn transcribe(engine: &mut dyn SttEngine, samples: &[f32], config: &Config) -> Result<String> {
    engine.set_vocabulary(&crate::controller::dictionary_words(config));
    let mut context = String::new();
    let mut texts = Vec::new();
    for piece in crate::controller::split_pieces(samples, crate::controller::piece_limit(config)) {
        engine.set_context(&context);
        let t = engine.transcribe(piece, &config.general.language)?;
        let t = t.text.trim().to_string();
        if !t.is_empty() {
            context = format!("{context} {t}");
            texts.push(t);
        }
    }
    engine.set_context("");
    Ok(texts.join(" "))
}

/// Words compared without case or punctuation, so only real mishearings count.
fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric() || *c == '\'')
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Word errors (substitutions, insertions and deletions) and the number of reference words.
pub fn word_errors(reference: &str, heard: &str) -> (usize, usize) {
    let r = words(reference);
    let h = words(heard);
    let mut prev: Vec<usize> = (0..=h.len()).collect();
    for (i, rw) in r.iter().enumerate() {
        let mut cur = vec![i + 1; h.len() + 1];
        for (j, hw) in h.iter().enumerate() {
            let sub = prev[j] + usize::from(rw != hw);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    (prev[h.len()], r.len())
}

/// Guided recording: read each sentence aloud; Enter starts and stops the mic.
fn record_set(dir: &Path) -> Result<ExitCode> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let config = vaani_core::paths::config_file()
        .and_then(|p| Config::load(&p).ok())
        .unwrap_or_default();
    let recorder = vaani_audio::Recorder::new();
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    println!(
        "Read each sentence the way you normally speak. Press Enter to start, Enter again \
         to stop. Type s then Enter to skip one, q to stop. Clips go to {}.",
        dir.display()
    );
    for (i, sentence) in SENTENCES.iter().enumerate() {
        let base = dir.join(format!("{:02}", i + 1));
        if base.with_extension("wav").exists() {
            continue;
        }
        println!();
        println!("{}/{}: {sentence}", i + 1, SENTENCES.len());
        print!("Enter to start> ");
        std::io::stdout().flush()?;
        match lines.next().transpose()?.as_deref().map(str::trim) {
            Some("q") | None => break,
            Some("s") => continue,
            _ => {}
        }
        let rec = recorder.start(Some(&config.audio.device))?;
        print!("Recording... Enter to stop> ");
        std::io::stdout().flush()?;
        let _ = lines.next();
        let samples = rec.finish()?;
        std::fs::write(base.with_extension("wav"), vaani_audio::wav_bytes(&samples))?;
        std::fs::write(base.with_extension("txt"), format!("{sentence}\n"))?;
        println!("Saved {:.1}s.", samples.len() as f32 / 16_000.0);
    }
    println!();
    println!("Done. Measure with: vaani bench {}", dir.display());
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_error_rate() {
        assert_eq!(word_errors("Hello, world.", "hello world"), (0, 2));
        assert_eq!(word_errors("send the report", "send a report"), (1, 3));
        assert_eq!(word_errors("send the report", "send report"), (1, 3));
        assert_eq!(word_errors("send report", "send the report now"), (2, 2));
        assert_eq!(word_errors("", "noise"), (1, 0));
    }
}
