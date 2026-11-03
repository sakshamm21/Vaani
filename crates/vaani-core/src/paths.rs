//! Where Vaani keeps its files: the XDG base directories on Linux, and on Windows
//! `%APPDATA%` for settings and `%LOCALAPPDATA%` for models, history and logs (they are
//! large or machine-specific, so they must not roam with the profile).

use std::path::PathBuf;

use directories::ProjectDirs;

fn dirs() -> Option<ProjectDirs> {
    // On Linux this gives ~/.config/vaani, ~/.local/share/vaani and ~/.local/state/vaani.
    ProjectDirs::from("io.github", "sakshamm21", "vaani")
}

/// `~/.config/vaani/config.toml`, or `None` if no home directory can be found.
pub fn config_file() -> Option<PathBuf> {
    dirs().map(|d| d.config_dir().join("config.toml"))
}

/// `~/.local/share/vaani`: history database, downloaded models.
pub fn data_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        dirs().map(|d| d.data_local_dir().to_path_buf())
    } else {
        dirs().map(|d| d.data_dir().to_path_buf())
    }
}

/// `~/.local/state/vaani/logs`.
pub fn log_dir() -> Option<PathBuf> {
    dirs().map(|d| {
        d.state_dir()
            .unwrap_or_else(|| d.data_local_dir())
            .join("logs")
    })
}
