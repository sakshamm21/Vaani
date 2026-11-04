//! Model registry, downloader and hardware detection.
//!
//! The registry lives in `models/registry.toml` and is compiled in. Models are downloaded to
//! `~/.local/share/vaani/models/<id>/`.

use std::fs;
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const REGISTRY: &str = include_str!("../../../models/registry.toml");
const SHERPA_RELEASES: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models";

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("unknown model `{0}`")]
    Unknown(String),
    #[error("download failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("no home directory found")]
    NoHome,
    #[error("download cancelled")]
    Cancelled,
    #[error("the download was corrupted (checksum mismatch); please try again")]
    Checksum,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    NemoTransducer,
    Whisper,
    Moonshine,
    SenseVoice,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub kind: ModelKind,
    pub archive: String,
    #[serde(default)]
    pub url: Option<String>,
    pub download_mb: u32,
    pub languages: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub recommended: bool,
    /// SHA-256 of the archive, checked after download.
    #[serde(default)]
    pub sha256: Option<String>,
}

impl ModelInfo {
    /// Whether the model knows many languages (and takes a language hint).
    pub fn is_multilingual(&self) -> bool {
        self.languages.contains("99 languages")
    }

    pub fn url(&self) -> String {
        self.url
            .clone()
            .unwrap_or_else(|| format!("{SHERPA_RELEASES}/{}.tar.bz2", self.archive))
    }

    /// Where this model lives once downloaded.
    pub fn dir(&self) -> Option<PathBuf> {
        models_dir().map(|d| d.join(&self.id))
    }

    pub fn is_installed(&self) -> bool {
        self.dir().is_some_and(|d| d.join(".complete").exists())
    }
}

#[derive(Deserialize)]
struct Registry {
    model: Vec<ModelInfo>,
}

/// Every model in the registry, in catalogue order.
pub fn registry() -> Vec<ModelInfo> {
    toml::from_str::<Registry>(REGISTRY)
        .map(|r| r.model)
        .unwrap_or_default()
}

pub fn find(id: &str) -> Option<ModelInfo> {
    registry().into_iter().find(|m| m.id == id)
}

pub fn models_dir() -> Option<PathBuf> {
    vaani_core::paths::data_dir().map(|d| d.join("models"))
}

/// Progress callback: (bytes done, bytes total if known). Return `false` to cancel.
pub type Progress<'a> = &'a mut dyn FnMut(u64, Option<u64>) -> bool;

/// Download and unpack a model.
///
/// The archive is saved next to the models folder as `.<id>.tar.bz2.part` and resumed with an
/// HTTP range request if the connection drops (several retries, then on the next attempt).
/// Its SHA-256 is checked against the registry before unpacking, and the archive is deleted
/// once the model is in place.
pub fn download(model: &ModelInfo, progress: Progress<'_>) -> Result<PathBuf, ModelError> {
    let dest = model.dir().ok_or(ModelError::NoHome)?;
    let parent = dest.parent().ok_or(ModelError::NoHome)?;
    fs::create_dir_all(parent)?;
    let part = parent.join(format!(".{}.tar.bz2.part", model.id));

    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("vaani/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(20))
        .timeout(None)
        .build()?;
    let mut attempt = 0;
    loop {
        match fetch_to(&client, &model.url(), &part, progress) {
            Ok(()) => break,
            Err(ModelError::Cancelled) => {
                let _ = fs::remove_file(&part);
                return Err(ModelError::Cancelled);
            }
            Err(e) if attempt < 5 => {
                attempt += 1;
                tracing::warn!(model = %model.id, attempt, "download interrupted ({e}); resuming");
                std::thread::sleep(std::time::Duration::from_secs(2 * attempt));
            }
            // Keep the partial file: the next attempt continues where this one stopped.
            Err(e) => return Err(e),
        }
    }

    if let Some(want) = &model.sha256 {
        let got = sha256_file(&part)?;
        if !got.eq_ignore_ascii_case(want) {
            tracing::error!(model = %model.id, %got, %want, "checksum mismatch");
            let _ = fs::remove_file(&part);
            return Err(ModelError::Checksum);
        }
        tracing::info!(model = %model.id, "checksum ok");
    }

    let tmp = parent.join(format!(".{}.partial", model.id));
    if tmp.exists() {
        fs::remove_dir_all(&tmp)?;
    }
    fs::create_dir_all(&tmp)?;
    let file = io::BufReader::new(fs::File::open(&part)?);
    if let Err(e) = tar::Archive::new(bzip2::read::BzDecoder::new(file)).unpack(&tmp) {
        let _ = fs::remove_dir_all(&tmp);
        let _ = fs::remove_file(&part);
        return Err(e.into());
    }
    let _ = fs::remove_file(&part);

    // Archives contain one top-level folder; flatten it.
    let mut entries: Vec<_> = fs::read_dir(&tmp)?.flatten().collect();
    let root = if entries.len() == 1 && entries[0].path().is_dir() {
        entries.remove(0).path()
    } else {
        tmp.clone()
    };
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    fs::rename(&root, &dest)?;
    if root != tmp {
        let _ = fs::remove_dir_all(&tmp);
    }
    // Test clips are not needed at runtime.
    let _ = fs::remove_dir_all(dest.join("test_wavs"));
    fs::write(dest.join(".complete"), model.archive.as_bytes())?;
    tracing::info!(model = %model.id, dir = %dest.display(), "model installed");
    Ok(dest)
}

/// Download `url` into `part`, continuing from its current length.
fn fetch_to(
    client: &reqwest::blocking::Client,
    url: &str,
    part: &Path,
    progress: Progress<'_>,
) -> Result<(), ModelError> {
    // Not append mode: Windows can't truncate a file opened for appending, which a restart
    // below needs. Writes go to the cursor, which starts at the end.
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .read(true)
        .open(part)?;
    let have = file.seek(io::SeekFrom::End(0))?;
    let mut req = client.get(url);
    if have > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let resp = req.send()?;
    if resp.status() == reqwest::StatusCode::RANGE_NOT_SATISFIABLE && have > 0 {
        // Already complete.
        return Ok(());
    }
    let resp = resp.error_for_status()?;
    let resumed = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    let start = if resumed {
        have
    } else {
        // The server ignored the range: start over.
        file.set_len(0)?;
        file.seek(io::SeekFrom::Start(0))?;
        0
    };
    let total = resp.content_length().map(|n| n + start);
    tracing::info!(%url, start, ?total, "downloading model");
    let mut reader = CountingReader {
        inner: resp,
        done: start,
        total,
        progress,
        cancelled: false,
    };
    let copied = io::copy(&mut reader, &mut file);
    if reader.cancelled {
        return Err(ModelError::Cancelled);
    }
    copied?;
    file.flush()?;
    if let Some(t) = total {
        if reader.done < t {
            return Err(ModelError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed early",
            )));
        }
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, ModelError> {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    let mut f = io::BufReader::new(fs::File::open(path)?);
    io::copy(&mut f, &mut h)?;
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

pub fn remove(model: &ModelInfo) -> Result<(), ModelError> {
    if let Some(d) = model.dir() {
        if d.exists() {
            fs::remove_dir_all(d)?;
        }
    }
    Ok(())
}

struct CountingReader<'a, R> {
    inner: R,
    done: u64,
    total: Option<u64>,
    progress: Progress<'a>,
    cancelled: bool,
}

impl<R: Read> Read for CountingReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.done += n as u64;
        if !(self.progress)(self.done, self.total) {
            self.cancelled = true;
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        Ok(n)
    }
}

/// Number of physical CPU cores (not SMT threads), from /proc/cpuinfo.
pub fn physical_cores() -> usize {
    let logical = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let Ok(info) = fs::read_to_string("/proc/cpuinfo") else {
        return logical;
    };
    let mut pairs = std::collections::HashSet::new();
    let (mut phys, mut core) = (None, None);
    for line in info.lines() {
        if let Some(v) = line.strip_prefix("physical id") {
            phys = v.split(':').nth(1).map(|s| s.trim().to_string());
        } else if let Some(v) = line.strip_prefix("core id") {
            core = v.split(':').nth(1).map(|s| s.trim().to_string());
        } else if line.trim().is_empty() {
            if let (Some(p), Some(c)) = (phys.take(), core.take()) {
                pairs.insert((p, c));
            }
        }
    }
    if pairs.is_empty() {
        logical
    } else {
        pairs.len()
    }
}

/// Default decoding threads. Measured on a Ryzen 5 5600U: 4 threads beat 12 by more than 2x.
pub fn default_threads() -> usize {
    physical_cores().clamp(1, 4)
}

/// Locate the files a model kind needs inside `dir`, preferring int8 variants.
pub fn find_file(dir: &Path, contains: &str) -> Option<PathBuf> {
    let mut matches: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains(contains) && n.ends_with(".onnx"))
        })
        .collect();
    matches.sort_by_key(|p| {
        let n = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        (!n.contains("int8"), n.len())
    });
    matches.into_iter().next()
}

pub fn find_tokens(dir: &Path) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with("tokens.txt"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_model_has_a_checksum() {
        for m in registry() {
            let h = m.sha256.unwrap_or_default();
            assert!(
                h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()),
                "{} has no valid sha256",
                m.id
            );
        }
    }

    #[test]
    fn registry_parses_and_ids_are_unique() {
        let r = registry();
        assert!(r.len() >= 10);
        let mut ids: Vec<_> = r.iter().map(|m| m.id.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), r.len());
        assert!(r.iter().filter(|m| m.recommended).count() == 1);
        assert!(find("parakeet-tdt-0.6b-v3").is_some());
    }

    #[test]
    fn default_model_in_config_exists() {
        let c = vaani_core::config::Config::default();
        assert!(find(&c.engine.local_model).is_some());
    }

    #[test]
    fn finds_int8_files_first() {
        let d = tempfile_dir();
        for f in [
            "tiny-encoder.onnx",
            "tiny-encoder.int8.onnx",
            "tiny-decoder.onnx",
            "tiny-tokens.txt",
        ] {
            fs::write(d.join(f), b"").unwrap();
        }
        assert!(
            find_file(&d, "encoder")
                .unwrap()
                .ends_with("tiny-encoder.int8.onnx")
        );
        assert!(
            find_file(&d, "decoder")
                .unwrap()
                .ends_with("tiny-decoder.onnx")
        );
        assert!(find_tokens(&d).unwrap().ends_with("tiny-tokens.txt"));
        let _ = fs::remove_dir_all(d);
    }

    fn tempfile_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("vaani-models-test-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        d
    }
}
