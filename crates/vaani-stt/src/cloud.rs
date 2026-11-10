//! Cloud speech-to-text: any OpenAI-compatible `/audio/transcriptions` endpoint (Groq,
//! OpenAI, and self-hosted servers such as speaches, LocalAI or whisper.cpp server), plus
//! Deepgram, ElevenLabs, AssemblyAI and Sarvam through their own APIs.

use std::time::{Duration, Instant};

use crate::{SttEngine, SttError, Transcript};

/// Wire format of a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Api {
    OpenAi,
    Deepgram,
    ElevenLabs,
    AssemblyAi,
    Sarvam,
}

#[derive(Debug, Clone)]
pub struct Provider {
    pub id: &'static str,
    pub api: Api,
    pub name: &'static str,
    pub base_url: &'static str,
    pub default_model: &'static str,
    pub key_url: &'static str,
}

pub const PROVIDERS: &[Provider] = &[
    Provider {
        id: "groq",
        api: Api::OpenAi,
        name: "Groq",
        base_url: "https://api.groq.com/openai/v1",
        default_model: "whisper-large-v3-turbo",
        key_url: "https://console.groq.com/keys",
    },
    Provider {
        id: "openai",
        api: Api::OpenAi,
        name: "OpenAI",
        base_url: "https://api.openai.com/v1",
        default_model: "gpt-4o-mini-transcribe",
        key_url: "https://platform.openai.com/api-keys",
    },
    Provider {
        id: "deepgram",
        api: Api::Deepgram,
        name: "Deepgram",
        base_url: "https://api.deepgram.com/v1",
        default_model: "nova-3",
        key_url: "https://console.deepgram.com/",
    },
    Provider {
        id: "elevenlabs",
        api: Api::ElevenLabs,
        name: "ElevenLabs",
        base_url: "https://api.elevenlabs.io/v1",
        default_model: "scribe_v1",
        key_url: "https://elevenlabs.io/app/settings/api-keys",
    },
    Provider {
        id: "assemblyai",
        api: Api::AssemblyAi,
        name: "AssemblyAI",
        base_url: "https://api.assemblyai.com/v2",
        default_model: "universal",
        key_url: "https://www.assemblyai.com/app/api-keys",
    },
    Provider {
        id: "sarvam",
        api: Api::Sarvam,
        name: "Sarvam (Indian languages)",
        base_url: "https://api.sarvam.ai",
        default_model: "saarika:v2.5",
        key_url: "https://dashboard.sarvam.ai/",
    },
    Provider {
        id: "openai_compatible",
        api: Api::OpenAi,
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
    /// The model was left at the provider's default, so Vaani may pick a better one for the
    /// language (Groq: full Whisper Large v3 for Hindi and Hinglish).
    model_is_default: bool,
    pub api_key: String,
    /// The user's Dictionary words, sent as hints where the API takes them.
    pub vocabulary: Vec<String>,
    /// What was said just before (the previous piece or dictation), for engines that take
    /// a prompt: they keep spelling, casing and language consistent with it.
    pub context: String,
    client: reqwest::blocking::Client,
    /// The time allowed for the request in flight, from the length of its audio.
    limit: Duration,
}

/// How long one cloud transcription may take before Vaani gives up on it (and uses the
/// local model if one is installed): 10 s, plus 1 s for every 3 s of audio, at most 2 min.
pub fn request_limit(samples_16k: usize) -> Duration {
    let audio_ms = samples_16k as u64 * 1000 / 16_000;
    Duration::from_millis((10_000 + audio_ms / 3).min(120_000))
}

/// Audio as it is uploaded.
struct Audio {
    bytes: Vec<u8>,
    mime: &'static str,
    file_name: &'static str,
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
            model_is_default: model.is_empty() || model == provider.default_model,
            api_key: api_key.into(),
            vocabulary: Vec::new(),
            context: String::new(),
            client,
            limit: Duration::from_secs(60),
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

    fn set_vocabulary(&mut self, words: &[String]) {
        self.vocabulary = words.to_vec();
    }

    fn set_context(&mut self, text: &str) {
        self.context = crate::context_tail(text);
    }

    fn warm(&mut self) {
        // Any cheap request opens the connection and finishes the TLS handshake; the client
        // keeps it for the transcription that follows. The answer itself doesn't matter.
        let t = Instant::now();
        let url = match self.provider.api {
            Api::OpenAi => format!("{}/models", self.base_url),
            _ => self.base_url.clone(),
        };
        let mut req = self.client.get(url).timeout(Duration::from_secs(2));
        if !self.api_key.is_empty() && self.provider.api == Api::OpenAi {
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
        self.limit = request_limit(samples.len());
        let audio = self.encode(samples);
        let hinglish = language.eq_ignore_ascii_case(crate::HINGLISH);
        let lang = crate::base_language(language);
        let multi = self.model.starts_with("nova-3") || self.model.starts_with("nova-2");
        let prompt = if self.instruction_prompt() {
            crate::instruction_prompt(language, &self.vocabulary, &self.context)
        } else {
            crate::prompt_with_context(language, &self.vocabulary, &self.context)
        };
        let text = match self.provider.api {
            Api::OpenAi => self.openai(audio, lang, &prompt)?,
            // Nova understands languages mixed in one sentence (Hinglish, or switching
            // between English and another language) as "multi"; detection picks just one.
            Api::Deepgram if multi && (hinglish || lang.is_none()) => {
                self.deepgram(audio, Some("multi"))?
            }
            Api::Deepgram => self.deepgram(audio, lang)?,
            // Scribe handles mixed speech best when it detects the languages itself.
            Api::ElevenLabs if hinglish => self.elevenlabs(audio, None)?,
            Api::ElevenLabs => self.elevenlabs(audio, lang)?,
            Api::AssemblyAi => self.assemblyai(audio, lang)?,
            Api::Sarvam => self.sarvam(audio, lang)?,
        };
        // A prompt-taking model sometimes repeats its prompt instead of hearing anything.
        if !prompt.is_empty() && crate::echoes_prompt(&text, &prompt) {
            tracing::debug!("cloud engine repeated its prompt; ignored");
            return Ok(Transcript {
                text: String::new(),
                took: t.elapsed(),
            });
        }
        Ok(Transcript {
            text,
            took: t.elapsed(),
        })
    }
}

fn audio_part(audio: Audio) -> Result<reqwest::blocking::multipart::Part, SttError> {
    reqwest::blocking::multipart::Part::bytes(audio.bytes)
        .file_name(audio.file_name)
        .mime_str(audio.mime)
        .map_err(|e| SttError::Engine(e.to_string()))
}

fn str_at(body: &serde_json::Value, pointer: &str) -> String {
    body.pointer(pointer)
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string()
}

impl CloudEngine {
    /// GPT-4o transcription models read their prompt as instructions; Whisper models read it
    /// as text that came just before the audio.
    fn instruction_prompt(&self) -> bool {
        self.provider.api == Api::OpenAi && self.model.starts_with("gpt-4o")
    }

    /// FLAC for the hosted services (about half the size of WAV, same audio, so long
    /// dictations upload faster); WAV for self-hosted servers and Sarvam.
    fn encode(&self, samples: &[f32]) -> Audio {
        let flac_ok = matches!(
            self.provider.id,
            "groq" | "openai" | "deepgram" | "elevenlabs" | "assemblyai"
        );
        if flac_ok {
            if let Some(bytes) = flac_16k(samples) {
                return Audio {
                    bytes,
                    mime: "audio/flac",
                    file_name: "audio.flac",
                };
            }
        }
        Audio {
            bytes: wav_16k(samples),
            mime: "audio/wav",
            file_name: "audio.wav",
        }
    }

    /// Send a request and return the JSON body, turning API errors into readable messages.
    fn call(&self, req: reqwest::blocking::RequestBuilder) -> Result<serde_json::Value, SttError> {
        let resp = req
            .timeout(self.limit)
            .send()
            .map_err(|e| SttError::Engine(format!("network: {e}")))?;
        let status = resp.status();
        let body: serde_json::Value = resp
            .json()
            .map_err(|e| SttError::Engine(format!("bad response ({status}): {e}")))?;
        if !status.is_success() {
            let msg = [
                "/error/message",
                "/error",
                "/detail/message",
                "/detail",
                "/err_msg",
                "/message",
            ]
            .iter()
            .find_map(|p| body.pointer(p).and_then(|m| m.as_str()))
            .unwrap_or("request failed");
            return Err(SttError::Engine(format!(
                "{} said: {msg} ({status})",
                self.provider.name
            )));
        }
        Ok(body)
    }

    fn deepgram(&self, audio: Audio, lang: Option<&str>) -> Result<String, SttError> {
        let mut query = vec![("model", self.model.as_str()), ("smart_format", "true")];
        match lang {
            Some(l) => query.push(("language", l)),
            None => query.push(("detect_language", "true")),
        }
        // Nova-3 takes the user's own words as key terms.
        if self.model.starts_with("nova-3") {
            for w in self
                .vocabulary
                .iter()
                .map(|w| w.trim())
                .filter(|w| !w.is_empty())
                .take(50)
            {
                query.push(("keyterm", w));
            }
        }
        let body = self.call(
            self.client
                .post(format!("{}/listen", self.base_url))
                .query(&query)
                .header("Authorization", format!("Token {}", self.api_key))
                .header("Content-Type", audio.mime)
                .body(audio.bytes),
        )?;
        Ok(str_at(
            &body,
            "/results/channels/0/alternatives/0/transcript",
        ))
    }

    fn elevenlabs(&self, audio: Audio, lang: Option<&str>) -> Result<String, SttError> {
        let mut form = reqwest::blocking::multipart::Form::new()
            .part("file", audio_part(audio)?)
            .text("model_id", self.model.clone())
            .text("tag_audio_events", "false");
        if let Some(l) = lang {
            form = form.text("language_code", l.to_string());
        }
        let body = self.call(
            self.client
                .post(format!("{}/speech-to-text", self.base_url))
                .header("xi-api-key", &self.api_key)
                .multipart(form),
        )?;
        Ok(str_at(&body, "/text"))
    }

    /// AssemblyAI has no one-shot endpoint: upload, ask for a transcript, then wait for it.
    fn assemblyai(&self, audio: Audio, lang: Option<&str>) -> Result<String, SttError> {
        let auth = |r: reqwest::blocking::RequestBuilder| r.header("Authorization", &self.api_key);
        let up = self.call(auth(
            self.client
                .post(format!("{}/upload", self.base_url))
                .header("Content-Type", "application/octet-stream")
                .body(audio.bytes),
        ))?;
        let audio_url = str_at(&up, "/upload_url");
        let mut job = serde_json::json!({
            "audio_url": audio_url,
            "speech_model": self.model,
            "punctuate": true,
            "format_text": true,
        });
        match lang {
            Some(l) => job["language_code"] = serde_json::json!(l),
            None => job["language_detection"] = serde_json::json!(true),
        }
        let started = self.call(auth(
            self.client
                .post(format!("{}/transcript", self.base_url))
                .json(&job),
        ))?;
        let id = str_at(&started, "/id");
        let deadline = Instant::now() + self.limit;
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(250));
            let r = self.call(auth(
                self.client
                    .get(format!("{}/transcript/{id}", self.base_url)),
            ))?;
            match r.get("status").and_then(|s| s.as_str()) {
                Some("completed") => return Ok(str_at(&r, "/text")),
                Some("error") => {
                    return Err(SttError::Engine(format!(
                        "AssemblyAI said: {}",
                        str_at(&r, "/error")
                    )));
                }
                _ => {}
            }
        }
        Err(SttError::Engine("AssemblyAI took too long".into()))
    }

    fn sarvam(&self, audio: Audio, lang: Option<&str>) -> Result<String, SttError> {
        // Sarvam wants codes such as "hi-IN"; "unknown" lets it detect the language.
        let code = match lang {
            Some(l) if l.contains('-') => l.to_string(),
            Some(l) => format!("{l}-IN"),
            None => "unknown".into(),
        };
        let form = reqwest::blocking::multipart::Form::new()
            .part("file", audio_part(audio)?)
            .text("model", self.model.clone())
            .text("language_code", code);
        let body = self.call(
            self.client
                .post(format!("{}/speech-to-text", self.base_url))
                .header("api-subscription-key", &self.api_key)
                .multipart(form),
        )?;
        Ok(str_at(&body, "/transcript"))
    }

    /// The model to ask for: on Groq with the default model, Hindi and Hinglish go to full
    /// Whisper Large v3, which hears Hindi clearly better than the Turbo version.
    fn model_for(&self, lang: Option<&str>) -> String {
        if self.provider.id == "groq" && self.model_is_default && lang == Some("hi") {
            return "whisper-large-v3".into();
        }
        self.model.clone()
    }

    fn openai(&self, audio: Audio, lang: Option<&str>, prompt: &str) -> Result<String, SttError> {
        let model = self.model_for(lang);
        if model == self.model {
            return self.openai_with(audio, &model, lang, prompt);
        }
        // The language-specific model may be gone from the provider; then use the usual one.
        let copy = Audio {
            bytes: audio.bytes.clone(),
            ..audio
        };
        self.openai_with(audio, &model, lang, prompt).or_else(|e| {
            // A network failure would only fail again, and slower.
            if e.to_string().contains("network:") {
                return Err(e);
            }
            tracing::warn!(%model, "{e}; trying {}", self.model);
            self.openai_with(copy, &self.model, lang, prompt)
        })
    }

    fn openai_with(
        &self,
        audio: Audio,
        model: &str,
        lang: Option<&str>,
        prompt: &str,
    ) -> Result<String, SttError> {
        let mut form = reqwest::blocking::multipart::Form::new()
            .part("file", audio_part(audio)?)
            .text("model", model.to_string())
            .text("response_format", "json");
        if let Some(l) = lang {
            form = form.text("language", l.to_string());
        }
        if !prompt.is_empty() {
            form = form.text("prompt", prompt.to_string());
        }
        let mut req = self
            .client
            .post(format!("{}/audio/transcriptions", self.base_url))
            .multipart(form);
        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }
        let body = self.call(req)?;
        Ok(str_at(&body, "/text"))
    }
}

/// 16 kHz mono 16-bit FLAC. None if the encoder fails (WAV is sent instead).
fn flac_16k(samples: &[f32]) -> Option<Vec<u8>> {
    use flacenc::component::BitRepr;
    use flacenc::error::Verify;
    if samples.is_empty() {
        return None;
    }
    let pcm: Vec<i32> = samples
        .iter()
        .map(|s| i32::from((s.clamp(-1.0, 1.0) * 32767.0) as i16))
        .collect();
    let config = flacenc::config::Encoder::default().into_verified().ok()?;
    let source = flacenc::source::MemSource::from_samples(&pcm, 1, 16, 16_000);
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size).ok()?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream.write(&mut sink).ok()?;
    Some(sink.as_slice().to_vec())
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
        let g = CloudEngine::new(provider("groq").unwrap(), "", "", "k").unwrap();
        assert_eq!(g.model_for(Some("hi")), "whisper-large-v3");
        assert_eq!(g.model_for(Some("en")), "whisper-large-v3-turbo");
        let pinned = CloudEngine::new(provider("groq").unwrap(), "", "distil", "k").unwrap();
        assert_eq!(pinned.model_for(Some("hi")), "distil");
    }

    #[test]
    fn flac_is_smaller_than_wav() {
        // One second of a voice-like tone.
        let s: Vec<f32> = (0..16_000)
            .map(|i| 0.3 * (i as f32 * 0.07).sin() * (i as f32 * 0.003).sin())
            .collect();
        let flac = flac_16k(&s).unwrap();
        assert_eq!(&flac[..4], b"fLaC");
        assert!(flac.len() < wav_16k(&s).len() * 3 / 4, "{}", flac.len());
    }

    #[test]
    fn limits_grow_with_audio() {
        assert_eq!(request_limit(16_000 * 3), Duration::from_secs(11));
        assert_eq!(request_limit(16_000 * 3000), Duration::from_secs(120));
    }

    #[test]
    fn wav_header() {
        let w = wav_16k(&[0.0; 10]);
        assert_eq!(w.len(), 64);
        assert_eq!(&w[..4], b"RIFF");
    }
}
