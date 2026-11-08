//! Model registry, downloader and hardware detection.
//!
//! The registry lives in `models/registry.toml` and is compiled in. Models are downloaded to
//! `~/.local/share/vaani/models/<id>/`.

use std::fs;
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    #[error(
        "the downloaded file did not match its checksum twice in a row; the model's publisher may have replaced it, and a Vaani update will pick up the new file"
    )]
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
/// The archive is saved next to the models folder as `.<id>.tar.bz2.part` and fetched in
/// ranges of [`CHUNK`] bytes, each with its own time limit, so a connection that stalls
/// (common on Wi-Fi and after a laptop wakes up) is dropped and reopened instead of hanging.
/// Every byte received is kept: a retry continues where the last one stopped, and retries go
/// on for as long as data keeps arriving. Only [`MAX_STALLS`] failures in a row with no
/// progress at all end the download, and the partial file is then kept for the next try.
/// The SHA-256 is checked against the registry before unpacking (a bad archive is fetched
/// again once, from scratch), and the archive is deleted only once the model is in place.
pub fn download(model: &ModelInfo, progress: Progress<'_>) -> Result<PathBuf, ModelError> {
    let dest = model.dir().ok_or(ModelError::NoHome)?;
    let parent = dest.parent().ok_or(ModelError::NoHome)?;
    fs::create_dir_all(parent)?;
    let part = partial_path(model).ok_or(ModelError::NoHome)?;
    let url = model.url();

    let mut refetched = false;
    loop {
        fetch_with_retries(&url, &part, &model.id, progress)?;
        let Some(want) = &model.sha256 else { break };
        let got = sha256_file(&part)?;
        if got.eq_ignore_ascii_case(want) {
            tracing::info!(model = %model.id, "checksum ok");
            break;
        }
        tracing::error!(model = %model.id, %got, %want, "checksum mismatch");
        let _ = fs::remove_file(&part);
        if refetched {
            return Err(ModelError::Checksum);
        }
        refetched = true;
    }

    install(model, &part, &dest)?;
    let _ = retry_io(|| fs::remove_file(&part));
    tracing::info!(model = %model.id, dir = %dest.display(), "model installed");
    Ok(dest)
}

/// Where a download in progress is kept. Its size says how much of the archive is there.
pub fn partial_path(model: &ModelInfo) -> Option<PathBuf> {
    models_dir().map(|d| d.join(format!(".{}.tar.bz2.part", model.id)))
}

/// Bytes of `model` already downloaded by an earlier, unfinished attempt.
pub fn partial_bytes(model: &ModelInfo) -> u64 {
    partial_path(model)
        .and_then(|p| fs::metadata(p).ok())
        .map_or(0, |m| m.len())
}

/// Bytes asked for in one request.
const CHUNK: u64 = 8 << 20;
/// Time limit for one request, connecting included. A request that runs out keeps what it
/// received, so a slow line still advances; a dead one is noticed within this time.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(45);
/// Failures in a row, with no byte received in between, before giving up (about 4 minutes
/// of waiting with the back-off below).
const MAX_STALLS: u32 = 12;

fn backoff(stalls: u32) -> Duration {
    Duration::from_secs(u64::from(stalls.min(5)).pow(2).clamp(1, 20))
}

fn fetch_with_retries(
    url: &str,
    part: &Path,
    id: &str,
    progress: Progress<'_>,
) -> Result<(), ModelError> {
    let build = |limit: Option<Duration>| {
        let b = reqwest::blocking::Client::builder()
            .user_agent(concat!("vaani/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(20))
            .tcp_keepalive(Duration::from_secs(15));
        match limit {
            Some(t) => b.timeout(t),
            None => b.timeout(None),
        }
        .build()
    };
    let ranged = build(Some(REQUEST_TIMEOUT))?;
    // Only for servers that ignore ranges: the whole file in one request, with no time limit.
    let whole = build(None)?;
    let mut ranges_ok = true;
    let mut stalls = 0;
    let mut total = None;
    loop {
        let before = file_len(part);
        let client = if ranges_ok { &ranged } else { &whole };
        match fetch_to(client, url, part, ranges_ok, &mut total, progress) {
            Ok(Fetched::Done) => return Ok(()),
            Ok(Fetched::More) => stalls = 0,
            Ok(Fetched::NoRanges) => ranges_ok = false,
            Err(ModelError::Cancelled) => {
                let _ = fs::remove_file(part);
                return Err(ModelError::Cancelled);
            }
            Err(e) => {
                if file_len(part) > before {
                    stalls = 0;
                } else {
                    stalls += 1;
                }
                if stalls > MAX_STALLS {
                    // Keep the partial file: the next attempt continues where this one stopped.
                    return Err(e);
                }
                // Data was still arriving: reconnect at once. Otherwise wait a little longer
                // each time (a network that is down, a laptop that just woke up).
                let wait = if stalls == 0 {
                    Duration::ZERO
                } else {
                    backoff(stalls)
                };
                tracing::warn!(model = %id, stalls, "download interrupted ({e}); resuming in {wait:?}");
                // Sleep in short steps so Cancel still answers quickly.
                let until = std::time::Instant::now() + wait;
                while std::time::Instant::now() < until {
                    if !progress(before.max(file_len(part)), total) {
                        let _ = fs::remove_file(part);
                        return Err(ModelError::Cancelled);
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
    }
}

fn file_len(p: &Path) -> u64 {
    fs::metadata(p).map_or(0, |m| m.len())
}

enum Fetched {
    /// The whole archive is on disk.
    Done,
    /// One range arrived; ask for the next.
    More,
    /// The server sent the whole file instead of a range; retry without ranges.
    NoRanges,
}

/// Fetch the next part of `url` into `part`, continuing from its current length.
fn fetch_to(
    client: &reqwest::blocking::Client,
    url: &str,
    part: &Path,
    ranges: bool,
    known_total: &mut Option<u64>,
    progress: Progress<'_>,
) -> Result<Fetched, ModelError> {
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
    if ranges {
        req = req.header(
            reqwest::header::RANGE,
            format!("bytes={have}-{}", have + CHUNK - 1),
        );
    } else if have > 0 {
        file.set_len(0)?;
        file.seek(io::SeekFrom::Start(0))?;
    }
    let resp = req.send()?;
    if resp.status() == reqwest::StatusCode::RANGE_NOT_SATISFIABLE && have > 0 {
        // Nothing past the end: the file is complete.
        return Ok(Fetched::Done);
    }
    let resp = resp.error_for_status()?;
    let partial = resp.status() == reqwest::StatusCode::PARTIAL_CONTENT;
    if ranges && !partial {
        // The server ignored the range. A complete file can't be streamed within one
        // request's time limit, so switch to whole-file requests.
        tracing::info!(%url, "server does not support ranges; downloading in one piece");
        return Ok(Fetched::NoRanges);
    }
    let total = if partial {
        resp.headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(content_range_total)
    } else {
        resp.content_length()
    };
    if total.is_some() {
        *known_total = total;
    }
    let start = if partial { have } else { 0 };
    let expected = resp.content_length();
    if have == 0 || !ranges {
        tracing::info!(%url, ?total, "downloading model");
    }
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
    let copied = copied?;
    file.flush()?;
    if expected.is_some_and(|n| copied < n) {
        return Err(ModelError::Io(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "connection closed early",
        )));
    }
    let done = start + copied;
    Ok(match total {
        Some(t) if done < t => Fetched::More,
        // Unknown total: a short range means the end was reached.
        None if partial && copied == CHUNK => Fetched::More,
        _ => Fetched::Done,
    })
}

/// The total size from a `Content-Range: bytes 0-99/1234` header.
fn content_range_total(v: &str) -> Option<u64> {
    v.rsplit('/').next()?.trim().parse().ok()
}

/// Unpack the archive at `part` into `dest`. The archive is left in place, so a failure here
/// (a full disk, a virus scanner holding a file) doesn't cost a new download.
fn install(model: &ModelInfo, part: &Path, dest: &Path) -> Result<(), ModelError> {
    let parent = dest.parent().ok_or(ModelError::NoHome)?;
    let tmp = parent.join(format!(".{}.partial", model.id));
    if tmp.exists() {
        retry_io(|| fs::remove_dir_all(&tmp))?;
    }
    fs::create_dir_all(&tmp)?;
    let file = io::BufReader::new(fs::File::open(part)?);
    if let Err(e) = tar::Archive::new(bzip2::read::BzDecoder::new(file)).unpack(&tmp) {
        let _ = retry_io(|| fs::remove_dir_all(&tmp));
        if model.sha256.is_none() {
            // Unverified and unreadable: most likely a bad download, so fetch it again.
            let _ = fs::remove_file(part);
        }
        return Err(e.into());
    }

    // Archives contain one top-level folder; flatten it.
    let mut entries: Vec<_> = fs::read_dir(&tmp)?.flatten().collect();
    let root = if entries.len() == 1 && entries[0].path().is_dir() {
        entries.remove(0).path()
    } else {
        tmp.clone()
    };
    if dest.exists() {
        retry_io(|| fs::remove_dir_all(dest))?;
    }
    // Windows: a virus scanner looking at the files just written makes a rename fail with
    // "access denied" for a moment, so try a few times.
    retry_io(|| fs::rename(&root, dest))?;
    if root != tmp {
        let _ = retry_io(|| fs::remove_dir_all(&tmp));
    }
    // Test clips are not needed at runtime.
    let _ = fs::remove_dir_all(dest.join("test_wavs"));
    fs::write(dest.join(".complete"), model.archive.as_bytes())?;
    Ok(())
}

/// Run a file operation, retrying for about 5 seconds while it fails with a sharing or
/// permission error (a scanner or indexer briefly holding the file, mostly on Windows).
fn held_by_another(e: &io::Error) -> bool {
    // 5 access denied, 32 sharing violation, 33 lock violation (Windows).
    matches!(
        e.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::ResourceBusy
    ) || (cfg!(windows) && matches!(e.raw_os_error(), Some(5 | 32 | 33)))
}

fn retry_io<T>(mut op: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut tries = 0;
    loop {
        match op() {
            Err(e) if tries < 20 && held_by_another(&e) => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(250));
            }
            r => return r,
        }
    }
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

    /// Downloads every archive and checks it against the registry (about 6 GB, so not part
    /// of the normal test run). The model publisher sometimes replaces archives, which makes
    /// every download fail its checksum; a weekly CI job runs this to catch that early.
    /// `cargo test -p vaani-models --release -- --ignored upstream`
    #[test]
    #[ignore = "downloads every model"]
    fn upstream_archives_match_registry() {
        use sha2::Digest;
        let client = reqwest::blocking::Client::builder()
            .timeout(None)
            .build()
            .unwrap();
        let mut bad = Vec::new();
        for m in registry() {
            let mut resp = client
                .get(m.url())
                .send()
                .unwrap()
                .error_for_status()
                .unwrap();
            let mut h = sha2::Sha256::new();
            io::copy(&mut resp, &mut h).unwrap();
            let got: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
            let want = m.sha256.clone().unwrap_or_default();
            println!("{} {}", m.id, if got == want { "ok" } else { "CHANGED" });
            if got != want {
                bad.push(format!("{}: sha256 is now {got}", m.id));
            }
        }
        assert!(
            bad.is_empty(),
            "update models/registry.toml:\n{}",
            bad.join("\n")
        );
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

    /// A tiny HTTP server for `data`. With `ranges`, it answers Range requests; every
    /// response is cut off after `cut` bytes the first `drops` times.
    fn serve(data: Vec<u8>, ranges: bool, cut: usize, drops: usize) -> String {
        use std::io::BufRead;
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            let mut dropped = 0;
            for conn in l.incoming() {
                let Ok(mut c) = conn else { continue };
                let mut r = io::BufReader::new(c.try_clone().unwrap());
                let mut range = None;
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        let (a, b) = v.trim().split_once('-').unwrap();
                        let a: usize = a.parse().unwrap();
                        let b: usize = b.parse().unwrap_or(data.len() - 1);
                        range = Some((a, b.min(data.len() - 1)));
                    }
                }
                let (head, body) = match range.filter(|_| ranges) {
                    Some((a, _)) if a >= data.len() => (
                        "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\n".to_string(),
                        &data[0..0],
                    ),
                    Some((a, b)) => (
                        format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {a}-{b}/{}\r\n",
                            b + 1 - a,
                            data.len()
                        ),
                        &data[a..=b],
                    ),
                    None => (
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n", data.len()),
                        &data[..],
                    ),
                };
                let _ = c.write_all(format!("{head}Connection: close\r\n\r\n").as_bytes());
                if dropped < drops && body.len() > cut {
                    dropped += 1;
                    let _ = c.write_all(&body[..cut]);
                } else {
                    let _ = c.write_all(body);
                }
            }
        });
        format!("http://{addr}/model.tar.bz2")
    }

    fn data(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 7 % 251) as u8).collect()
    }

    #[test]
    fn resumes_after_dropped_connections() {
        let want = data((CHUNK as usize) * 2 + 12_345);
        let url = serve(want.clone(), true, 100_000, 3);
        let d = tempfile_dir().join("resume");
        let _ = fs::create_dir_all(&d);
        let part = d.join("a.part");
        let _ = fs::remove_file(&part);
        let mut last = (0, None);
        let mut cb = |done, total| {
            last = (done, total);
            true
        };
        fetch_with_retries(&url, &part, "test", &mut cb).unwrap();
        assert_eq!(fs::read(&part).unwrap(), want);
        assert_eq!(last.1, Some(want.len() as u64));
        // Already complete: a second call only confirms it.
        fetch_with_retries(&url, &part, "test", &mut |_, _| true).unwrap();
        assert_eq!(fs::read(&part).unwrap(), want);
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn works_with_servers_that_ignore_ranges() {
        let want = data(300_000);
        let url = serve(want.clone(), false, 0, 0);
        let d = tempfile_dir().join("noranges");
        let _ = fs::create_dir_all(&d);
        let part = d.join("b.part");
        fs::write(&part, b"stale bytes from an older try").unwrap();
        fetch_with_retries(&url, &part, "test", &mut |_, _| true).unwrap();
        assert_eq!(fs::read(&part).unwrap(), want);
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn reads_content_range_totals() {
        assert_eq!(content_range_total("bytes 0-99/1234"), Some(1234));
        assert_eq!(content_range_total("bytes 0-99/*"), None);
    }

    fn tempfile_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("vaani-models-test-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        d
    }
}
