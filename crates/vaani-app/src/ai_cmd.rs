//! `vaani cleanup`: run the AI clean-up outside a dictation.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use vaani_core::config::Config;

pub fn run(text: Option<String>, file: Option<PathBuf>, models: bool) -> Result<ExitCode> {
    let cfg = vaani_core::paths::config_file()
        .and_then(|p| Config::load(&p).ok())
        .unwrap_or_default();
    let c = &cfg.cleanup;
    let Some(provider) = vaani_llm::provider(&c.ai_provider) else {
        bail!("unknown AI provider \"{}\"", c.ai_provider);
    };
    if cfg.privacy.local_only && !provider.local {
        bail!("\"Keep everything on this computer\" is on; only Ollama can be used");
    }
    let key = if provider.key_id.is_empty() {
        String::new()
    } else {
        vaani_store::secrets::get_key(provider.key_id)
            .with_context(|| format!("no {} API key saved", provider.name))?
    };
    if models {
        for id in vaani_llm::list_models(provider, &c.ai_base_url, &key)? {
            println!("{id}");
        }
        return Ok(ExitCode::SUCCESS);
    }
    let dictionary: Vec<String> = cfg
        .dictionary
        .replacements
        .iter()
        .map(|r| r.to.clone())
        .collect();
    let req = vaani_llm::Request {
        provider,
        base_url: &c.ai_base_url,
        model: &c.ai_model,
        api_key: &key,
        prompt: &c.ai_prompt,
        style: vaani_llm::Style::from_id(&c.style),
        extra: &c.ai_instruction,
        dictionary: &dictionary,
        timeout: Duration::from_secs(15),
    };
    let lines: Vec<String> = match (text, file) {
        (Some(t), _) => vec![t],
        (None, Some(f)) => std::fs::read_to_string(&f)
            .with_context(|| format!("reading {}", f.display()))?
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(String::from)
            .collect(),
        (None, None) => bail!("give some text, or --file"),
    };
    let (mut applied, mut same, mut rejected) = (0, 0, 0);
    for line in &lines {
        let mut t = Instant::now();
        // Free tiers allow a few requests a minute; wait out the limit instead of skipping lines.
        let mut result = vaani_llm::clean(line, &req);
        for _ in 0..5 {
            let Err(vaani_llm::LlmError::RateLimited(_, hint)) = &result else {
                break;
            };
            std::thread::sleep(hint.unwrap_or(Duration::from_secs(5)) + Duration::from_millis(250));
            // Time only the request that answered, not the wait.
            t = Instant::now();
            result = vaani_llm::clean(line, &req);
        }
        let ms = t.elapsed().as_millis();
        match result {
            Ok(out) if out == *line => {
                same += 1;
                println!("IN   {line}\nSAME ({ms} ms)\n");
            }
            Ok(out) => {
                applied += 1;
                println!("IN   {line}\nOUT  {out}  ({ms} ms)\n");
            }
            Err(e) => {
                rejected += 1;
                println!("IN   {line}\nKEPT ({e}, {ms} ms)\n");
            }
        }
    }
    if lines.len() > 1 {
        println!("{applied} cleaned, {same} returned unchanged, {rejected} kept as transcribed");
    }
    Ok(ExitCode::SUCCESS)
}
