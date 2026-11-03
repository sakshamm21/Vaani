//! `vaani models ...` and `vaani history`.

use std::io::Write;
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Result, bail};
use vaani_core::config::Config;

pub fn list() -> Result<ExitCode> {
    let current = vaani_core::paths::config_file()
        .and_then(|p| Config::load(&p).ok())
        .unwrap_or_default()
        .engine
        .local_model;
    println!("{:<28} {:>8}  {:<10} LANGUAGES", "ID", "SIZE", "STATUS");
    for m in vaani_models::registry() {
        let status = if m.is_installed() {
            if m.id == current {
                "in use"
            } else {
                "installed"
            }
        } else if m.id == current {
            "selected"
        } else {
            ""
        };
        println!(
            "{:<28} {:>5} MB  {:<10} {}{}",
            m.id,
            m.download_mb,
            status,
            m.languages,
            if m.recommended { "  (recommended)" } else { "" }
        );
    }
    Ok(ExitCode::SUCCESS)
}

pub fn download(id: &str, use_it: bool) -> Result<ExitCode> {
    crate::logging::init_cli();
    let Some(model) = vaani_models::find(id) else {
        bail!("unknown model `{id}`; see `vaani models list`");
    };
    if model.is_installed() {
        println!("{} is already downloaded.", model.name);
    } else {
        println!(
            "Downloading {} (about {} MB)...",
            model.name, model.download_mb
        );
        let start = Instant::now();
        let mut last = Instant::now();
        let mut cb = |done: u64, total: Option<u64>| {
            if last.elapsed().as_millis() > 200 {
                last = Instant::now();
                let mb = done as f64 / 1e6;
                match total {
                    Some(t) => print!(
                        "\r  {:>6.1} / {:.1} MB ({:>3.0}%)",
                        mb,
                        t as f64 / 1e6,
                        done as f64 * 100.0 / t as f64
                    ),
                    None => print!("\r  {mb:>6.1} MB"),
                }
                let _ = std::io::stdout().flush();
            }
            true
        };
        let dir = vaani_models::download(&model, &mut cb)?;
        println!(
            "\nInstalled in {} ({} s).",
            dir.display(),
            start.elapsed().as_secs()
        );
    }
    if use_it {
        let path =
            vaani_core::paths::config_file().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
        let mut c = Config::load(&path).unwrap_or_default();
        c.engine.local_model = model.id.clone();
        c.save(&path)?;
        println!("Vaani will now use {}.", model.name);
        let _ = crate::ipc::try_call(crate::ipc::Call::ReloadEngine);
    }
    Ok(ExitCode::SUCCESS)
}

pub fn remove(id: &str) -> Result<ExitCode> {
    let Some(model) = vaani_models::find(id) else {
        bail!("unknown model `{id}`");
    };
    vaani_models::remove(&model)?;
    println!("Removed {}.", model.name);
    Ok(ExitCode::SUCCESS)
}

pub fn history(n: usize) -> Result<ExitCode> {
    let Some(path) = vaani_core::paths::data_dir().map(|d| d.join("history.db")) else {
        bail!("no home directory");
    };
    let h = vaani_store::History::open(&path)?;
    for e in h.recent(n)?.into_iter().rev() {
        println!(
            "{}  {}",
            if e.inserted { " " } else { "!" },
            e.text.trim_end()
        );
    }
    Ok(ExitCode::SUCCESS)
}
