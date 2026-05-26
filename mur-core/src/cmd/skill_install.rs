//! Skill install orchestrator — resolve source, fetch, verify, store, trust, lock.

use anyhow::{Context, Result, anyhow, bail};
use std::path::Path;

use mur_common::skill::{
    SkillLock, SkillManifest, TrustLevel, content_hash_for_trust, content_sha256, global_skill_dir,
    lockfile, scan::scan_skill, write_to_dir,
};
use mur_common::trust::skills::{SkillTrustStore, TrustEntry};

use crate::cmd::agent::resolve_mur_home;
use crate::cmd::skill_registry;
use crate::cmd::skill_resolver::{self, ResolveSource, ResolvedNode, ResolverInput};

/// Pure entry point — takes explicit home + registry_url. Used by tests and future M4 code.
pub fn cmd_install(home: &Path, registry_url: &str, source: &str) -> Result<()> {
    // M4a: agent://<agent-name>/<skill-name> — peer transfer pull.
    if let Some(rest) = source.strip_prefix("agent://") {
        let (agent_name, skill_name) = rest.split_once('/').ok_or_else(|| {
            anyhow!("invalid agent:// URL '{source}' — expected agent://<agent-name>/<skill-name>")
        })?;
        if agent_name.is_empty() || skill_name.is_empty() {
            bail!("invalid agent:// URL '{source}' — agent name and skill name must be non-empty");
        }
        return install_from_agent(home, agent_name, skill_name);
    }

    let src_path = Path::new(source);

    let (reg_dir, _idx) =
        skill_registry::fetch_and_load(home, registry_url).context("fetch registry")?;

    let input = ResolverInput {
        mur_home: home.to_path_buf(),
        registry_dir: reg_dir,
    };

    let source_enum = if src_path.exists() && src_path.is_file() {
        ResolveSource::LocalFile(src_path)
    } else {
        ResolveSource::RegistryLatest(source)
    };

    let order = skill_resolver::resolve(&input, source_enum)?;
    if order.is_empty() {
        bail!("resolver returned empty install order");
    }

    // Install leaves first. The root is the last entry.
    for node in &order {
        install_resolved_node(home, node)?;
    }

    // Write lock at the root skill dir.
    let root = order.last().unwrap();
    let lock = SkillLock {
        schema_version: lockfile::SCHEMA_VERSION,
        installed_at: chrono::Utc::now().to_rfc3339(),
        locked: order
            .iter()
            .map(|n| (n.name.clone(), n.version.to_string()))
            .collect(),
    };
    let root_dir = global_skill_dir(home, &root.name);
    lock.write(&root_dir).context("write skill.lock")?;

    println!("installed: {} v{}", root.name, root.version);
    if order.len() > 1 {
        println!("  + {} transitive dependencies", order.len() - 1);
    }

    // Best-effort: embed every installed skill for vector dedup.
    for node in &order {
        try_embed_skill(home, &node.name, &node.version.to_string(), &node.manifest);
    }

    Ok(())
}

fn install_resolved_node(home: &Path, node: &ResolvedNode) -> Result<()> {
    let report = scan_skill(&node.manifest)?;
    let dir = global_skill_dir(home, &node.name);
    write_to_dir(&dir, &node.manifest)?;
    let hash = content_sha256(&node.manifest)?;
    let mut trust = SkillTrustStore::load(home).map_err(|e| anyhow::anyhow!("load trust: {e}"))?;
    let level = if report.has_blocking_findings() {
        TrustLevel::Sandboxed
    } else {
        TrustLevel::Verified
    };
    trust.insert(
        hash,
        TrustEntry {
            name: node.name.clone(),
            version: node.version.to_string(),
            level,
            installed_at: chrono::Utc::now().to_rfc3339(),
            publisher: Some(node.manifest.publisher.clone()),
        },
    );
    trust
        .save(home)
        .map_err(|e| anyhow::anyhow!("save trust: {e}"))?;
    if report.has_blocking_findings() {
        eprintln!(
            "⚠ {} v{}: security findings — installed Sandboxed",
            node.name, node.version
        );
        for line in report.human_summary() {
            eprintln!("    {line}");
        }
    }
    Ok(())
}

fn install_from_agent(home: &Path, agent_name: &str, skill_name: &str) -> Result<()> {
    // 1. Discover — confirm the named agent is registered on this host.
    let agent_dir = home.join("agents").join(agent_name);
    if !agent_dir.exists() {
        bail!(
            "agent '{agent_name}' not found at {} — cannot dial",
            agent_dir.display()
        );
    }

    // 2. Pull — JSON-RPC `skills/get` to the source agent.
    tracing::info!(skill = %skill_name, source = %agent_name, "pulling skill via A2A");
    use crate::a2a_dial::{DialMode, dial_method};
    use mur_common::skill::parse_canonical;

    let response = dial_method(
        home,
        agent_name,
        "skills/get",
        serde_json::json!({"skill": skill_name}),
        DialMode::Auto,
    )
    .with_context(|| format!("pull agent://{agent_name}/{skill_name}"))?;

    let manifest_yaml = response
        .get("manifest")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            anyhow!("agent://{agent_name}/{skill_name}: response missing 'manifest' field")
        })?;
    let mut manifest: SkillManifest = parse_canonical(manifest_yaml)
        .with_context(|| format!("parse manifest from agent://{agent_name}/{skill_name}"))?;

    let advertised_hash = response
        .get("content_sha256")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            anyhow!("agent://{agent_name}/{skill_name}: response missing 'content_sha256'")
        })?;
    let received_hash =
        content_hash_for_trust(&manifest).map_err(|e| anyhow!("hash received manifest: {e}"))?;

    // The sender's advertised hash must match what we just computed.
    // Otherwise the payload was tampered with in transit or the sender is
    // buggy — either way, refuse the install rather than poisoning the
    // trust store.
    if advertised_hash != received_hash {
        bail!(
            "agent://{agent_name}/{skill_name}: hash mismatch \
             (sender advertised {advertised_hash}, computed {received_hash}) — install blocked"
        );
    }

    // 3. Verify — content-based trust.
    let trust_level = resolve_agent_install_trust(home, &manifest, &received_hash)?;

    // 4. Append transfer chain.
    manifest
        .transfer_chain
        .push(format!("agent://{agent_name}"));

    // 5. Re-scan the post-mutation manifest.
    let report = scan_skill(&manifest).context("scan manifest")?;
    let effective_level = if report.has_blocking_findings() {
        TrustLevel::Sandboxed
    } else {
        trust_level
    };

    // 6. Install — write to the target store.
    let dir = global_skill_dir(home, &manifest.name);
    write_to_dir(&dir, &manifest).context("write installed skill")?;

    // 7. Record in trust store.
    let trust_key =
        content_hash_for_trust(&manifest).map_err(|e| anyhow!("hash trust key: {e}"))?;
    let mut trust = SkillTrustStore::load(home).map_err(|e| anyhow!("load trust: {e}"))?;
    trust.insert(
        trust_key,
        TrustEntry {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            level: effective_level,
            installed_at: chrono::Utc::now().to_rfc3339(),
            publisher: Some(manifest.publisher.clone()),
        },
    );
    trust.save(home).map_err(|e| anyhow!("save trust: {e}"))?;

    // 8. Register in the calling agent's profile.
    if let Some(caller) = caller_agent_name(home)? {
        register_in_profile(home, &caller, &manifest)?;
    }

    if report.has_blocking_findings() {
        eprintln!(
            "⚠ {} v{}: security findings — installed Sandboxed",
            manifest.name, manifest.version
        );
        for line in report.human_summary() {
            eprintln!("    {line}");
        }
    }

    println!(
        "installed: {} v{} ({effective_level:?}, from agent://{agent_name})",
        manifest.name, manifest.version,
    );
    if effective_level == TrustLevel::Sandboxed {
        println!(
            "hint: run `mur skill trust {} verified` after review",
            manifest.name
        );
    }

    // Best-effort: embed for vector dedup.
    try_embed_skill(home, &manifest.name, &manifest.version, &manifest);

    Ok(())
}

/// Content-based trust for agent-installed skills.
/// Order: revocation > registry hash match > default Sandboxed.
fn resolve_agent_install_trust(
    home: &Path,
    manifest: &SkillManifest,
    received_hash: &str,
) -> Result<TrustLevel> {
    let trust_store = SkillTrustStore::load(home).map_err(|e| anyhow!("load trust: {e}"))?;
    if trust_store.is_revoked(received_hash) {
        bail!(
            "skill '{}' (hash {}) is revoked — install blocked",
            manifest.name,
            received_hash
        );
    }

    let cache_dir = crate::cmd::skill_registry::registry_cache_dir(home);
    if cache_dir.exists()
        && let Ok(idx) = crate::cmd::skill_registry::load_index(&cache_dir)
        && let Some(entry) = idx.skills.get(&manifest.name)
        && entry.content_sha256 == received_hash
    {
        return Ok(TrustLevel::Verified);
    }

    Ok(TrustLevel::Sandboxed)
}

/// Resolve the agent that issued this install command.
fn caller_agent_name(home: &Path) -> Result<Option<String>> {
    let Some(name) = std::env::var("MUR_AGENT_NAME").ok() else {
        return Ok(None);
    };
    let agent_dir = home.join("agents").join(&name);
    if !agent_dir.exists() {
        bail!(
            "MUR_AGENT_NAME='{name}' but {} does not exist",
            agent_dir.display()
        );
    }
    Ok(Some(name))
}

/// Push (or update) a `SkillCardEntry` into the calling agent's profile.
fn register_in_profile(home: &Path, agent_name: &str, m: &SkillManifest) -> Result<()> {
    use mur_common::agent::{SkillCardEntry, SkillCardTrigger};

    let profile_path = home.join("agents").join(agent_name).join("profile.yaml");
    let text = std::fs::read_to_string(&profile_path)
        .with_context(|| format!("read {}", profile_path.display()))?;
    let mut profile: mur_common::AgentProfile = serde_yaml_ng::from_str(&text)
        .with_context(|| format!("parse {}", profile_path.display()))?;

    let entry = SkillCardEntry {
        name: m.name.clone(),
        version: m.version.clone(),
        publisher: m.publisher.clone(),
        description: m.description.clone(),
        category: serde_yaml_ng::to_string(&m.category)
            .unwrap_or_default()
            .trim()
            .to_string(),
        tags: m.tags.clone(),
        triggers: m
            .triggers
            .iter()
            .map(|t| SkillCardTrigger {
                kind: serde_yaml_ng::to_string(&t.kind)
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
                pattern: t.pattern.clone().unwrap_or_default(),
            })
            .collect(),
        abstract_text: m.content.r#abstract.clone(),
        transfer_chain: m.transfer_chain.clone(),
    };

    if let Some(slot) = profile
        .installed_skills
        .iter_mut()
        .find(|e| e.name == entry.name)
    {
        *slot = entry;
    } else {
        profile.installed_skills.push(entry);
    }

    // Atomic write — temp file + rename.
    let tmp = profile_path.with_extension("yaml.tmp");
    let yaml = serde_yaml_ng::to_string(&profile)?;
    std::fs::write(&tmp, yaml).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &profile_path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), profile_path.display()))?;
    Ok(())
}

/// CLI shim — resolves MUR_HOME and MUR_SKILL_REGISTRY_URL from env.
pub fn cmd_install_cli(source: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    let registry_url = std::env::var("MUR_SKILL_REGISTRY_URL")
        .unwrap_or_else(|_| skill_registry::DEFAULT_REGISTRY.to_string());
    cmd_install(&home, &registry_url, source)
}

/// Pure update — re-resolves to latest versions.
pub fn cmd_update(home: &Path, registry_url: &str, name: &str) -> Result<()> {
    cmd_install(home, registry_url, name)?;
    println!("updated: {name}");
    Ok(())
}

/// CLI shim for update.
pub fn cmd_update_cli(name: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    let registry_url = std::env::var("MUR_SKILL_REGISTRY_URL")
        .unwrap_or_else(|_| skill_registry::DEFAULT_REGISTRY.to_string());
    cmd_update(&home, &registry_url, name)
}

/// Best-effort embedding after install. Failure is non-fatal — the skill is
/// usable without an embedding (Jaccard path still works), and reindex-vec
/// can backfill.
fn try_embed_skill(home: &Path, name: &str, version: &str, manifest: &SkillManifest) {
    use crate::store::embedding::EmbeddingConfig;

    let cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
    let embed_config = EmbeddingConfig::from_config(&cfg);
    let index_dir = home.join("lance");

    let handle = match tokio::runtime::Handle::try_current() {
        Ok(h) => h,
        Err(_) => return,
    };
    let result = handle.block_on(async {
        let store = crate::store::vector::factory::get_vector_store(&cfg, &index_dir).await?;
        crate::skill_index::embed_manifest_and_upsert(
            manifest,
            name,
            version,
            &embed_config,
            &*store,
        )
        .await
    });

    match result {
        Ok(dims) => {
            eprintln!("indexed skill '{name}': {dims}-dim embedding");
        }
        Err(e) => {
            eprintln!(
                "warning: failed to index skill '{name}' for vector dedup: {e} \
                 (run `mur skill reindex-vec` to backfill)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn valid_yaml_parses_and_validates() {
        let yaml = r#"
name: test
version: 1.0.0
publisher: human:t
description: t
category: context
content:
  abstract: a
  context: b
"#;
        let m = mur_common::skill::parse_canonical(yaml).unwrap();
        mur_common::skill::validate(&m).unwrap();
    }

    #[test]
    fn malformed_agent_urls_are_rejected() {
        let home = tempdir().unwrap();
        let cases = [
            ("agent://noslash", "expected agent://"),
            ("agent://", "expected agent://"),
            ("agent:///emptyagent", "non-empty"),
            ("agent://emptyskill/", "non-empty"),
        ];
        for (input, expected_substr) in cases {
            let err = cmd_install(home.path(), "https://example.com/registry", input).unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains(expected_substr),
                "case {input:?}: expected '{expected_substr}' in '{msg}'"
            );
        }
    }
}
