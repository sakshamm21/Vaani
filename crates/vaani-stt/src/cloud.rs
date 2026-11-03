//! Cloud speech-to-text through any OpenAI-compatible `/audio/transcriptions` endpoint:
//! Groq, OpenAI, and self-hosted servers (speaches, LocalAI, whisper.cpp server...).

use std::time::{Duration, Instant};

use crate::{SttEngine, SttError, Transcript};

#[derive(Debug, Clone)]
pub struct Provider {
    pub id: &'static str,
    pub name: &'static str,
    pub base_url: &'static str,
    pub default_model: &'static str,
    pub key_url: &'static str,
}

pub const PROVIDERS: &[Provider] = &[
    Provider {
        id: "groq",
        name: "Groq",
        base_url: "https://api.groq.com/openai/v1",
        default_model: "whisper-large-v3-turbo",
        key_url: "https://console.groq.com/keys",
    },
    Provider {
        id: "openai",
        name: "OpenAI",
        base_url: "https://api.openai.com/v1",
        default_model: "gpt-4o-mini-transcribe",
        key_url: "https://platform.openai.com/api-keys",
    },
    Provider {
        id: "openai_compatible",
        name: "Other (OpenAI-compatible)",
        base_url: "http://localhost:8000/v1",
        default_model: "whisper-1",
        key_url: "",
    },
];

pub fn provider(id: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|p| p.id == id)
}

pub struct CloudEngine {
    pub provider: &'static Provider,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    client: reqwest::blocking::Client,
}

impl CloudEngine {
    pub fn new(
        provider: &'static Provider,
        base_url: &str,
        model: &str,
        api_key: &str,
    ) -> Result<Self, SttError> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(8))
            .pool_idle_timeout(Duration::from_secs(600))
            .tcp_keepalive(Duration::from_secs(30))
            .build()
            .map_err(|e| SttError::NotReady(e.to_string()))?;
        Ok(Self {
            provider,
            base_url: if base_url.is_empty() {
                provider.base_url.into()
            } else {
                base_url.trim_end_matches('/').into()
            },
            model: if model.is_empty() {
                provider.default_model.into()
            } else {
                model.into()
            },
            api_key: api_key.into(),
            client,
        })
    }
}

impl SttEngine for CloudEngine {
    fn name(&self) -> String {
        format!("{} {} (cloud)", self.provider.name, self.model)
    }

    fn is_cloud(&self) -> bool {
        true
    }

    fn warm(&mut self) {
        // Any cheap request opens the connection and finishes the TLS handshake; the client
        // keeps it for the transcription that follows. The answer itself doesn't matter.
        let t = Instant::now();
        let mut req = self
            .client
            .get(format!("{}/models", self.base_url))
            .timeout(Duration::from_secs(2));
        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }
        let ok = req.send().map(|r| r.status().as_u16()).unwrap_or(0);
        tracing::debug!(
            status = ok,
            ms = t.elapsed().as_millis() as u64,
            "cloud connection warmed"
        );
    }

    fn transcribe(&mut self, samples: &[f32], language: &str) -> Result<Transcript, SttError> {
        let t = Instant::now();
        let wav = wav_16k(samples);
        let part = reqwest::blocking::multipart::Part::bytes(wav)
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .map_err(|e| SttError::Engine(e.to_string()))?;
        let mut form = reqwest::blocking::multipart::Form::new()
            .part("file", part)
            .text("model", self.model.clone())
            .text("response_format", "json");
        if !language.is_empty() && language != "auto" {
            form = form.text("language", language.to_string());
        }
        let mut req = self
            .client
            .post(format!("{}/audio/transcriptions", self.base_url))
            .multipart(form);
        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }
        let resp = req
            .send()
            .map_err(|e| SttError::Engine(format!("network: {e}")))?;
        let status = resp.status();
        let body: serde_json::Value = resp
            .json()
            .map_err(|e| SttError::Engine(format!("bad response ({status}): {e}")))?;
        if !status.is_success() {
            let msg = body
                .pointer("/error/message")
                .and_then(|m| m.as_str())
                .unwrap_or("request failed");
            return Err(SttError::Engine(format!(
                "{} said: {msg} ({status})",
                self.provider.name
            )));
        }
        let text = body
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string();
        Ok(Transcript {
            text,
            took: t.elapsed(),
        })
    }
}

fn wav_16k(samples: &[f32]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + data_len as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&1u16.to_le_bytes()); // mono
    b.extend_from_slice(&16_000u32.to_le_bytes());
    b.extend_from_slice(&32_000u32.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        b.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_have_defaults() {
        assert_eq!(
            provider("groq").unwrap().default_model,
            "whisper-large-v3-turbo"
        );
        let e = CloudEngine::new(
            provider("openai_compatible").unwrap(),
            "http://x/v1/",
            "",
            "",
        )
        .unwrap();
        assert_eq!(e.base_url, "http://x/v1");
        assert_eq!(e.model, "whisper-1");
        assert!(e.is_cloud());
    }

    #[test]
    fn wav_header() {
        let w = wav_16k(&[0.0; 10]);
        assert_eq!(w.len(), 64);
        assert_eq!(&w[..4], b"RIFF");
    }
}
