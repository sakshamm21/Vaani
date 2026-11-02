//! The user's config file: schema, load, save, validation, migration and hot reload.
//!
//! The file is plain TOML at `~/.config/vaani/config.toml`. A missing file means "all defaults".
//! A file written by an older Vaani is migrated in memory and saved back in the new format.
//! API keys never live here; they go to the system keyring (see `vaani-store`).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use notify::{RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};

/// Bump this when a change to the schema needs a migration, and add a step to [`migrate`].
pub const CURRENT_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} is not valid TOML: {message}")]
    Parse { path: PathBuf, message: String },
    #[error(
        "{path} was written by a newer Vaani (config version {found}, this build understands up to {CURRENT_VERSION})"
    )]
    TooNew { path: PathBuf, found: u32 },
    #[error("invalid setting `{field}`: {reason}")]
    Invalid { field: &'static str, reason: String },
    #[error("could not watch config file: {0}")]
    Watch(#[from] notify::Error),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub general: General,
    pub hotkey: Hotkey,
    pub audio: Audio,
    pub engine: Engine,
    pub cleanup: Cleanup,
    pub insert: Insert,
    pub privacy: Privacy,
    pub logging: Logging,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            general: General::default(),
            hotkey: Hotkey::default(),
            audio: Audio::default(),
            engine: Engine::default(),
            cleanup: Cleanup::default(),
            insert: Insert::default(),
            privacy: Privacy::default(),
            logging: Logging::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// Spoken language: "auto" or a BCP-47 code such as "en", "hi".
    pub language: String,
    pub autostart: bool,
    /// Play short sounds when dictation starts and stops.
    pub sounds: bool,
    /// Show the floating pill while dictating.
    pub overlay: bool,
    /// Look of the settings window and pill.
    pub theme: Theme,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    /// Follow the desktop's light or dark setting.
    #[default]
    System,
    Light,
    Dark,
}

impl Default for General {
    fn default() -> Self {
        Self {
            language: "auto".into(),
            autostart: true,
            sounds: true,
            overlay: true,
            theme: Theme::System,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyMode {
    /// Hold to talk, release to finish. A quick tap starts hands-free dictation instead;
    /// tap again to finish.
    PushToTalk,
    /// Press once to start, again to finish.
    Toggle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyBackend {
    /// Pick by probing the session (the normal choice).
    Auto,
    X11,
    Portal,
    Evdev,
    /// Only the `vaani toggle` CLI, bound in the desktop's own shortcut settings.
    Cli,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hotkey {
    pub mode: HotkeyMode,
    /// Key or chord, e.g. "RightCtrl", "Super+Space".
    pub key: String,
    pub backend: HotkeyBackend,
}

impl Default for Hotkey {
    fn default() -> Self {
        Self {
            mode: HotkeyMode::PushToTalk,
            key: "RightCtrl".into(),
            backend: HotkeyBackend::Auto,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Audio {
    /// Microphone name as listed in settings; empty means the system default.
    pub device: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineKind {
    Local,
    Cloud,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Engine {
    pub kind: EngineKind,
    /// An id from `models/registry.toml`.
    pub local_model: String,
    /// A provider id such as "groq" or "openai_compatible".
    pub cloud_provider: String,
    pub cloud_model: String,
    /// Base URL for the "openai_compatible" provider; empty uses the provider's default.
    pub cloud_base_url: String,
    /// Decoding threads for local models; 0 picks automatically (physical cores, at most 4).
    pub threads: u32,
}

impl Default for Engine {
    fn default() -> Self {
        Self {
            kind: EngineKind::Local,
            local_model: "parakeet-tdt-0.6b-v3".into(),
            cloud_provider: "groq".into(),
            cloud_model: "whisper-large-v3-turbo".into(),
            cloud_base_url: String::new(),
            threads: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Cleanup {
    /// Rule-based cleanup is always on; this switches the optional AI pass.
    pub ai_enabled: bool,
    /// Hard timeout for AI cleanup, after which the rule-cleaned text is used.
    pub ai_timeout_ms: u64,
    /// Drop "um", "uh" and similar.
    pub remove_fillers: bool,
}

impl Default for Cleanup {
    fn default() -> Self {
        Self {
            ai_enabled: false,
            ai_timeout_ms: 1500,
            remove_fillers: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsertMethod {
    Auto,
    Paste,
    Type,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Insert {
    pub method: InsertMethod,
    /// Put the user's clipboard back after pasting.
    pub restore_clipboard: bool,
    /// Add a space after each dictation so the next one doesn't run into it.
    pub trailing_space: bool,
}

impl Default for Insert {
    fn default() -> Self {
        Self {
            method: InsertMethod::Auto,
            restore_clipboard: true,
            trailing_space: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Privacy {
    /// Block every network call (cloud engines and AI cleanup are refused).
    pub local_only: bool,
    /// Don't save dictations to history (same as `keep_history = 0`).
    pub incognito: bool,
    /// How many dictations History keeps: the newest N, 0 for none, -1 for all.
    pub keep_history: i64,
}

impl Default for Privacy {
    fn default() -> Self {
        Self {
            local_only: false,
            incognito: false,
            keep_history: 20,
        }
    }
}

impl Privacy {
    /// Effective limit: `None` keeps everything, `Some(0)` keeps nothing.
    pub fn history_limit(&self) -> Option<usize> {
        if self.incognito {
            return Some(0);
        }
        usize::try_from(self.keep_history).ok()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Logging {
    /// A `tracing` filter: "error", "warn", "info", "debug", "trace" or a directive list.
    pub level: String,
}

impl Default for Logging {
    fn default() -> Self {
        Self {
            level: "info".into(),
        }
    }
}

impl Config {
    /// Check values that parse but make no sense.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.privacy.keep_history < -1 {
            return Err(ConfigError::Invalid {
                field: "privacy.keep_history",
                reason: "must be a number of dictations, 0 for none, or -1 for all".into(),
            });
        }
        if self.general.language.trim().is_empty() {
            return Err(ConfigError::Invalid {
                field: "general.language",
                reason: "must be \"auto\" or a language code such as \"en\"".into(),
            });
        }
        if self.hotkey.key.trim().is_empty() {
            return Err(ConfigError::Invalid {
                field: "hotkey.key",
                reason: "must name a key, e.g. \"RightCtrl\"".into(),
            });
        }
        if !(100..=10_000).contains(&self.cleanup.ai_timeout_ms) {
            return Err(ConfigError::Invalid {
                field: "cleanup.ai_timeout_ms",
                reason: format!(
                    "{} is out of range (100 to 10000)",
                    self.cleanup.ai_timeout_ms
                ),
            });
        }
        if self.engine.local_model.trim().is_empty() {
            return Err(ConfigError::Invalid {
                field: "engine.local_model",
                reason: "must be a model id from the registry".into(),
            });
        }
        if self.privacy.local_only && self.engine.kind == EngineKind::Cloud {
            return Err(ConfigError::Invalid {
                field: "engine.kind",
                reason: "\"cloud\" cannot be used while privacy.local_only is on".into(),
            });
        }
        Ok(())
    }

    /// Parse config text. `path` is only used in error messages.
    ///
    /// Returns the config and whether a migration changed it (so the caller can save it back).
    pub fn from_toml(text: &str, path: &Path) -> Result<(Self, bool), ConfigError> {
        let mut value: toml::Table =
            text.parse()
                .map_err(|e: toml::de::Error| ConfigError::Parse {
                    path: path.to_path_buf(),
                    message: e.message().to_string(),
                })?;
        let migrated = migrate(&mut value, path)?;
        let config: Config =
            toml::Value::Table(value)
                .try_into()
                .map_err(|e: toml::de::Error| ConfigError::Parse {
                    path: path.to_path_buf(),
                    message: e.message().to_string(),
                })?;
        config.validate()?;
        Ok((config, migrated))
    }

    pub fn to_toml(&self) -> String {
        // Serializing plain structs of strings, bools, ints and enums cannot fail.
        toml::to_string_pretty(self).unwrap_or_default()
    }

    /// Load from `path`. A missing file gives the defaults. A migrated file is saved back.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(ConfigError::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        let (config, migrated) = Self::from_toml(&text, path)?;
        if migrated {
            tracing::info!(path = %path.display(), "migrated config to version {CURRENT_VERSION}");
            config.save(path)?;
        }
        Ok(config)
    }

    /// Validate, then write atomically (temp file in the same folder, then rename) so a crash
    /// mid-write never leaves a half-written config.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        self.validate()?;
        let werr = |source| ConfigError::Write {
            path: path.to_path_buf(),
            source,
        };
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(dir).map_err(werr)?;
        let tmp = path.with_extension("toml.tmp");
        {
            let mut f = fs::File::create(&tmp).map_err(werr)?;
            f.write_all(self.to_toml().as_bytes()).map_err(werr)?;
            f.sync_all().map_err(werr)?;
        }
        fs::rename(&tmp, path).map_err(werr)?;
        Ok(())
    }
}

/// Bring an older config table up to [`CURRENT_VERSION`], step by step.
/// Returns whether anything changed.
fn migrate(table: &mut toml::Table, path: &Path) -> Result<bool, ConfigError> {
    let found = match table.get("version") {
        None => 0,
        Some(v) => v
            .as_integer()
            .and_then(|i| u32::try_from(i).ok())
            .ok_or_else(|| ConfigError::Invalid {
                field: "version",
                reason: "must be a whole number".into(),
            })?,
    };
    if found > CURRENT_VERSION {
        return Err(ConfigError::TooNew {
            path: path.to_path_buf(),
            found,
        });
    }
    let mut version = found;
    while version < CURRENT_VERSION {
        // One step per version. Version 0 is a hand-written file with no `version` key and
        // needs nothing else. Future steps go here, e.g. `if version == 1 { rename a key }`.
        version += 1;
    }
    if version != found {
        table.insert("version".into(), toml::Value::Integer(i64::from(version)));
        return Ok(true);
    }
    Ok(false)
}

/// Watches the config file and calls `on_change` with the newly loaded config whenever the
/// file's content changes. Invalid edits are reported as errors; the caller keeps the old config.
///
/// Dropping the watcher stops watching.
pub struct ConfigWatcher {
    _watcher: notify::RecommendedWatcher,
}

impl ConfigWatcher {
    pub fn spawn<F>(path: PathBuf, initial: Config, mut on_change: F) -> Result<Self, ConfigError>
    where
        F: FnMut(Result<Config, ConfigError>) + Send + 'static,
    {
        let (tx, rx) = mpsc::channel::<()>();
        let file_name = path.file_name().map(|n| n.to_os_string());
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(event) = res {
                    let ours = event
                        .paths
                        .iter()
                        .any(|p| p.file_name().map(|n| n.to_os_string()) == file_name);
                    if ours {
                        let _ = tx.send(());
                    }
                }
            })?;
        // Watch the folder, not the file: editors save by renaming a new file over the old one.
        let dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        fs::create_dir_all(&dir).map_err(|source| ConfigError::Write {
            path: dir.clone(),
            source,
        })?;
        watcher.watch(&dir, RecursiveMode::NonRecursive)?;

        thread::Builder::new()
            .name("config-watch".into())
            .spawn(move || {
                let mut current = initial;
                while rx.recv().is_ok() {
                    // Debounce: editors fire several events per save.
                    thread::sleep(Duration::from_millis(150));
                    while rx.try_recv().is_ok() {}
                    match Config::load(&path) {
                        Ok(new) if new == current => {}
                        Ok(new) => {
                            current = new.clone();
                            on_change(Ok(new));
                        }
                        Err(e) => on_change(Err(e)),
                    }
                }
            })
            .map_err(|source| ConfigError::Read {
                path: PathBuf::from("config-watch thread"),
                source,
            })?;

        Ok(Self { _watcher: watcher })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn p() -> &'static Path {
        Path::new("test.toml")
    }

    #[test]
    fn defaults_are_valid_and_round_trip() {
        let c = Config::default();
        c.validate().unwrap();
        let (back, migrated) = Config::from_toml(&c.to_toml(), p()).unwrap();
        assert_eq!(back, c);
        assert!(!migrated);
    }

    #[test]
    fn partial_file_fills_in_defaults() {
        let (c, _) = Config::from_toml("version = 1\n[hotkey]\nmode = \"toggle\"\n", p()).unwrap();
        assert_eq!(c.hotkey.mode, HotkeyMode::Toggle);
        assert_eq!(c.hotkey.key, "RightCtrl");
        assert_eq!(c.engine, Engine::default());
    }

    #[test]
    fn missing_version_is_migrated() {
        let (c, migrated) = Config::from_toml("[general]\nlanguage = \"hi\"\n", p()).unwrap();
        assert!(migrated);
        assert_eq!(c.version, CURRENT_VERSION);
        assert_eq!(c.general.language, "hi");
    }

    #[test]
    fn newer_version_is_refused() {
        let err = Config::from_toml("version = 99\n", p()).unwrap_err();
        assert!(matches!(err, ConfigError::TooNew { found: 99, .. }));
    }

    #[test]
    fn bad_values_are_rejected() {
        let err =
            Config::from_toml("version = 1\n[cleanup]\nai_timeout_ms = 5\n", p()).unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                field: "cleanup.ai_timeout_ms",
                ..
            }
        ));

        let err = Config::from_toml(
            "version = 1\n[engine]\nkind = \"cloud\"\n[privacy]\nlocal_only = true\n",
            p(),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ConfigError::Invalid {
                field: "engine.kind",
                ..
            }
        ));

        let err = Config::from_toml("version = 1\n[hotkey]\nmode = \"hold\"\n", p()).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn unknown_keys_are_ignored() {
        // A file from a newer minor release with extra keys should still load.
        let (c, _) = Config::from_toml("version = 1\nfuture_thing = true\n", p()).unwrap();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn load_missing_file_gives_defaults_and_save_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.toml");
        assert_eq!(Config::load(&path).unwrap(), Config::default());

        let mut c = Config::default();
        c.general.language = "en".into();
        c.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), c);
        assert!(!path.with_extension("toml.tmp").exists());
    }

    #[test]
    fn load_saves_back_migrated_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[general]\nautostart = true\n").unwrap();
        let c = Config::load(&path).unwrap();
        assert!(c.general.autostart);
        let on_disk = fs::read_to_string(&path).unwrap();
        assert!(on_disk.contains("version = 1"));
    }

    #[test]
    fn save_refuses_invalid_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut c = Config::default();
        c.hotkey.key = " ".into();
        assert!(c.save(&path).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn watcher_reports_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let initial = Config::default();
        initial.save(&path).unwrap();

        let (tx, rx) = mpsc::channel();
        let _w = ConfigWatcher::spawn(path.clone(), initial.clone(), move |r| {
            let _ = tx.send(r.map_err(|e| e.to_string()));
        })
        .unwrap();

        thread::sleep(Duration::from_millis(100));
        let mut changed = initial;
        changed.hotkey.mode = HotkeyMode::Toggle;
        changed.save(&path).unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let got = loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(r) => break r.unwrap(),
                Err(_) if Instant::now() < deadline => continue,
                Err(_) => panic!("no change event"),
            }
        };
        assert_eq!(got.hotkey.mode, HotkeyMode::Toggle);

        fs::write(&path, "version = 1\n[cleanup]\nai_timeout_ms = 1\n").unwrap();
        let err = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(err.is_err());
    }
}
