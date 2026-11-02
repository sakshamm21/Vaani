//! Speech-to-text engines.
//!
//! [`SttEngine`] is the one interface the app uses. [`local::LocalEngine`] runs models in the
//! supervised `vaani-worker` process; cloud providers implement the same trait.

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
}
