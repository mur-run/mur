//! Startup helpers: agent-home discovery, model resolution, parent pid.

use super::*;

/// How long to wait for one user secret to come back from the keychain before
/// giving up on it and starting anyway. Short on purpose: the only thing that
/// makes this slow is a modal prompt, and a prompt nobody answers must not
/// cost the agent its startup.
pub(super) const USER_SECRET_RESOLVE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(3);

pub(super) fn parent_pid() -> u32 {
    #[cfg(unix)]
    unsafe {
        libc::getppid() as u32
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// Extract the binary's embedded agent (idempotent across runs) and
/// return the resolved agent_home directory.
pub(super) fn resolve_embedded_agent_home() -> anyhow::Result<PathBuf> {
    #[cfg(feature = "embedded-agent")]
    {
        use crate::export::bin_embed::EMBEDDED_TAR;
        use crate::export::extract::{default_cache_base, extract_embedded_to};
        let info = extract_embedded_to(EMBEDDED_TAR, &default_cache_base())?;
        Ok(info.agent_home)
    }
    #[cfg(not(feature = "embedded-agent"))]
    {
        anyhow::bail!("embedded-agent feature not compiled in")
    }
}

/// `--load` path: install a `.muragent` into `mur_home/agents/<slug>` (reusing
/// the shared installer — validation, trust, extraction) and return that home.
/// Stashes the slug as the expected name so the post-load name check passes.
pub(super) fn load_muragent_and_home(path: &str, mur_home: &Path) -> anyhow::Result<PathBuf> {
    use mur_common::muragent::installer;
    use mur_common::muragent::reader::MuragentArchive;

    let archive = MuragentArchive::read(Path::new(path))
        .map_err(|e| anyhow::anyhow!("read .muragent at {path}: {e}"))?;
    let outcome = installer::install(&archive, mur_home, "cli")
        .map_err(|e| anyhow::anyhow!("install .muragent: {e}"))?;
    let slug = outcome.manifest.agent.slug.clone();
    // SAFETY: single-threaded startup, before any tokio tasks spawn.
    unsafe {
        std::env::set_var("MUR_RUNTIME_EXPECTED_NAME", &slug);
    }
    Ok(mur_home.join("agents").join(&slug))
}

pub(super) fn read_flag_profile_from_args() -> anyhow::Result<String> {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--profile"
            && let Some(name) = args.next()
        {
            return Ok(name);
        }
        if let Some(n) = a.strip_prefix("--profile=") {
            return Ok(n.to_string());
        }
    }
    anyhow::bail!("bare mur-agent-runtime requires --profile <name>")
}

/// Resolve the effective `ModelEntry` for an agent. Prefers
/// `profile.model_ref` (looks it up in `~/.mur/models.yaml`); falls back to
/// the inline `model:` block when the field is unset.
pub fn resolve_model_entry(
    profile: &mur_common::agent::AgentProfile,
) -> anyhow::Result<mur_common::model::ModelEntry> {
    use anyhow::Context;
    use mur_common::model::{ModelEntry, ModelRegistry};
    if let Some(name) = profile.model_ref.as_deref() {
        let path = ModelRegistry::default_path()?;
        let reg = ModelRegistry::load_from(&path)
            .with_context(|| format!("load registry {}", path.display()))?;
        let entry = reg
            .models
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("model_ref {name:?} not found in {}", path.display()))?;
        Ok(entry.clone())
    } else {
        Ok(ModelEntry {
            provider: profile.model.provider.clone(),
            model: profile.model.name.clone(),
            base_url: None,
            secret: None,
            capabilities: vec![],
            params: serde_json::to_value(&profile.model.params).unwrap_or(serde_json::Value::Null),
            tier: None,
            cost_per_1k_tokens: None,
            ..Default::default()
        })
    }
}

/// §6: the old per-agent caps are loaded and ignored. One line per key at
/// start, never an error — nobody's agent stops starting over a stale key.
pub fn stale_cap_warnings(hitl: &mur_common::agent::HitlConfig) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(n) = hitl.max_iterations {
        out.push(format!(
            "profile.yaml hitl.max_iterations: {n} — IGNORED since 2.79; remove it (bounds are `mur limits <agent>`: deadline / stuck)"
        ));
    }
    if let Some(n) = hitl.max_tokens {
        out.push(format!(
            "profile.yaml hitl.max_tokens: {n} — IGNORED since 2.79; remove it (bounds are `mur limits <agent>`: deadline / stuck / cost_usd)"
        ));
    }
    out
}
