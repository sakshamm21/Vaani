//! Speech-to-text engines.
//!
//! [`SttEngine`] is the one interface the app uses. [`local::LocalEngine`] runs models in the
//! supervised `vaani-worker` process; cloud providers implement the same trait.

pub mod cloud;
pub mod local;
pub mod protocol;

use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum SttError {
    #[error("speech engine is not ready: {0}")]
    NotReady(String),
    #[error("speech engine failed: {0}")]
    Engine(String),
    #[error("worker I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("worker protocol: {0}")]
    Protocol(#[from] postcard::Error),
}

#[derive(Debug, Clone)]
pub struct Transcript {
    pub text: String,
    pub took: Duration,
}

pub trait SttEngine: Send {
    /// A short name for logs and the UI, e.g. "Parakeet TDT 0.6B v3 (local)".
    fn name(&self) -> String;
    /// Transcribe 16 kHz mono audio.
    fn transcribe(&mut self, samples_16k: &[f32], language: &str) -> Result<Transcript, SttError>;
    /// Does audio leave the machine? Drives the cloud icon in the overlay.
    fn is_cloud(&self) -> bool {
        false
    }
    /// Names and words to listen for (the user's Dictionary). Engines that can take hints
    /// use them; the rest ignore them.
    fn set_vocabulary(&mut self, _words: &[String]) {}
    /// Called when a recording starts, so the engine can get ready while the user speaks
    /// (a cloud engine opens its connection then, instead of after the key is released).
    fn warm(&mut self) {}
}

/// The language setting for Hindi mixed with English, written in Latin letters.
pub const HINGLISH: &str = "hinglish";

/// The plain language code for an engine: None for automatic detection, "hi" for Hinglish,
/// and "hi" for "hi-IN".
pub fn base_language(language: &str) -> Option<&str> {
    let l = language.trim();
    if l.is_empty() || l.eq_ignore_ascii_case("auto") {
        return None;
    }
    if l.eq_ignore_ascii_case(HINGLISH) {
        return Some("hi");
    }
    Some(l.split(['-', '_']).next().unwrap_or(l))
}

/// A sample of the expected output, given to engines that accept a prompt. Whisper-style
/// models copy the style of their prompt, so this asks for Hindi in Latin letters with
/// English words kept in English, which is how people type Hinglish.
pub const HINGLISH_PROMPT: &str = "Haan yaar, main kal office aa raha hoon. Meeting 11 baje \
hai, toh please report ready rakhna. Kya scene hai, sab theek? Okay, done, I'll call you later.";

/// The prompt for engines that accept one: the Hinglish style sample when asked for, and the
/// user's own words so they are spelled right. Empty when there is nothing to say.
pub fn prompt(language: &str, vocabulary: &[String]) -> String {
    let mut out = String::new();
    if language.eq_ignore_ascii_case(HINGLISH) {
        out.push_str(HINGLISH_PROMPT);
    }
    let words: Vec<&str> = vocabulary
        .iter()
        .map(|w| w.trim())
        .filter(|w| !w.is_empty())
        .take(40)
        .collect();
    if !words.is_empty() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&words.join(", "));
        out.push('.');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages() {
        assert_eq!(base_language("auto"), None);
        assert_eq!(base_language(""), None);
        assert_eq!(base_language("hinglish"), Some("hi"));
        assert_eq!(base_language("hi-IN"), Some("hi"));
        assert_eq!(base_language("en"), Some("en"));
    }

    #[test]
    fn prompts() {
        assert_eq!(prompt("en", &[]), "");
        assert_eq!(prompt("en", &["Vaani".into(), " ".into()]), "Vaani.");
        assert!(prompt("hinglish", &["Vaani".into()]).ends_with("later. Vaani."));
    }
}
