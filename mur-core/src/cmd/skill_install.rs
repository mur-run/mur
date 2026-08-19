//! Skill install orchestrator — resolve source, fetch, verify, store, trust, lock.

use anyhow::{Context, Result, anyhow, bail};
use std::path::Path;

use mur_common::skill::credit::{CreditEntry, CreditEvidence, CreditKind};
use mur_common::skill::{
    SkillLock, SkillManifest, TrustLevel, content_hash_for_trust, global_skill_dir, lockfile,
    scan::scan_skill, write_to_dir,
};
use mur_common::trust::skills::{SkillTrustStore, TrustEntry};

use crate::cmd::agent::resolve_mur_home;
use crate::cmd::skill_registry;
use crate::cmd::skill_resolver::{self, ResolveSource, ResolvedNode, ResolverInput};
use crate::cross_agent::credit::ledger as credit_ledger;
use crate::cross_agent::propagate::InstallContext;

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
        install_resolved_node(home, &input.registry_dir, node)?;
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

/// Same as `cmd_install` but accepts an explicit `InstallContext` so the
/// credit ledger can attribute the install correctly.
///
/// Used by `mur agent propagate` for auto-propagation; the public
/// `cmd_install` wrapper passes `InstallContext::Manual`.
pub fn cmd_install_ctx(
    home: &Path,
    registry_url: &str,
    source: &str,
    caller_agent: &str,
    ctx: InstallContext,
) -> Result<()> {
    cmd_install(home, registry_url, source)?;

    // Determine skill name + version for the ledger entry.
    if let Some(rest) = source.strip_prefix("agent://") {
        let (agent_name, skill_name) = rest
            .split_once('/')
            .ok_or_else(|| anyhow!("invalid agent:// URL: {source}"))?;
        let dir = global_skill_dir(home, skill_name);
        let manifest_path = dir.join("skill.yaml");
        let version = std::fs::read_to_string(&manifest_path)
            .ok()
            .and_then(|s| serde_yaml_ng::from_str::<SkillManifest>(&s).ok())
            .map(|m| m.version)
            .unwrap_or_else(|| "0.0.0".into());

        let (evidence, kind) = match &ctx {
            InstallContext::AutoPropagate {
                source_fitness,
                source_samples,
            } => (
                Some(CreditEvidence::Propagator {
                    from_agent: agent_name.to_string(),
                    fitness_at_install: *source_fitness,
                    samples_at_install: *source_samples,
                }),
                CreditKind::Propagator,
            ),
            InstallContext::Manual => (
                Some(CreditEvidence::Propagator {
                    from_agent: agent_name.to_string(),
                    fitness_at_install: 0.0,
                    samples_at_install: 0,
                }),
                CreditKind::Propagator,
            ),
        };

        let entry = CreditEntry {
            ts: chrono::Utc::now(),
            skill: skill_name.to_string(),
            skill_version: version,
            kind,
            agent: caller_agent.to_string(),
            evidence,
            source: format!("agent://{agent_name}"),
        };
        if let Err(e) = credit_ledger::append(home, caller_agent, &entry) {
            tracing::warn!("credit ledger append failed for {}: {e}", entry.skill);
        }
    } else {
        // Registry or local install — Author kind on the calling agent.
        let src_path = std::path::Path::new(source);
        let (name, version) = if src_path.exists() && src_path.is_file() {
            let bytes = std::fs::read(src_path)?;
            let m: SkillManifest = serde_yaml_ng::from_slice(&bytes)?;
            (m.name, m.version)
        } else {
            let dir = global_skill_dir(home, source);
            let manifest_path = dir.join("skill.yaml");
            let bytes = std::fs::read(&manifest_path)
                .with_context(|| format!("read manifest at {}", manifest_path.display()))?;
            let m: SkillManifest = serde_yaml_ng::from_slice(&bytes)?;
            (m.name, m.version)
        };

        let entry = CreditEntry {
            ts: chrono::Utc::now(),
            skill: name,
            skill_version: version,
            kind: CreditKind::Author,
            agent: caller_agent.to_string(),
            evidence: None,
            source: format!("human:{caller_agent}"),
        };
        if let Err(e) = credit_ledger::append(home, caller_agent, &entry) {
            tracing::warn!("credit ledger append failed for {}: {e}", entry.skill);
        }
    }
    Ok(())
}

/// Resolve `<home>/skills/<name>` for install, refusing a skill whose name
/// would escape the skills dir. The name comes from a fetched manifest (a
/// registry/git source the user chose, but not necessarily one they vetted),
/// so an unsafe `../…` name must not be written to an arbitrary path.
fn safe_skill_dir(home: &Path, name: &str) -> Result<std::path::PathBuf> {
    if !mur_common::skill::loader::is_valid_skill_name(name) {
        anyhow::bail!("refusing to install skill with unsafe name {name:?} (path traversal)");
    }
    Ok(global_skill_dir(home, name))
}

/// Stamp a registry-sourced manifest with its origin before writing to disk.
/// Drives `mur skill upgrade`. Called ONLY for skills resolved from the
/// registry cache — never for local-path or `agent://` peer-transfer
/// installs, which have no registry version to upgrade against.
pub fn stamp_registry_origin(m: &mut SkillManifest) {
    m.origin = Some(format!("registry:{}/{}", m.publisher, m.name));
    m.origin_version = Some(m.version.clone());
    m.origin_hash = mur_common::skill::hash::content_hash_for_origin(m).ok();
}

fn install_resolved_node(home: &Path, registry_dir: &Path, node: &ResolvedNode) -> Result<()> {
    let report = scan_skill(&node.manifest)?;
    let dir = safe_skill_dir(home, &node.name)?;
    let mut manifest = node.manifest.clone();
    // Registry-resolved nodes always load their yaml from inside the
    // registry cache dir (root via RegistryLatest, transitive deps always
    // via pick_best). A LocalFile root's yaml_path lives outside it.
    if node.yaml_path.starts_with(registry_dir) {
        stamp_registry_origin(&mut manifest);
    }
    write_to_dir(&dir, &manifest)?;
    // Trust-store key: the trust hash, so a later transfer or generation
    // increment does not re-key this entry (see `content_hash_for_trust`).
    let hash = content_hash_for_trust(&node.manifest)?;
    let mut trust = SkillTrustStore::load(home).map_err(|e| anyhow::anyhow!("load trust: {e}"))?;
    let level = if report.has_blocking_findings() {
        TrustLevel::Sandboxed
    } else {
        TrustLevel::Verified
    };
    let installed_at = chrono::Utc::now().to_rfc3339();
    let publisher = Some(node.manifest.publisher.clone());
    trust.insert(
        hash.clone(),
        TrustEntry {
            name: node.name.clone(),
            version: node.version.to_string(),
            level,
            installed_at: installed_at.clone(),
            publisher: publisher.clone(),
            // Hash-keyed entry: the KEY is the content hash, so the field would
            // only repeat it. The drift baseline is the name-keyed entry below.
            ..Default::default()
        },
    );
    // Drift baseline, keyed by NAME (#960). The hash-keyed entry above is the
    // load-time allow-list: it answers "is this exact content trusted", and by
    // construction it can never notice that the content changed — a new version
    // simply lands under a new key. Comparing against the LAST install needs an
    // entry that survives the version change, which is what keying by name buys.
    //
    // `content_sha256` is populated in the trust domain, matching what
    // `registry-add` pins, so the two install paths compare like with like. No
    // signer fingerprint: this path has the security scan but not a verified
    // publisher signature, so a publisher-change claim would be unfounded.
    // `check_drift` skips the publisher comparison when it is absent and still
    // performs the content and rollback checks.
    trust.entries.insert(
        node.name.clone(),
        TrustEntry {
            name: node.name.clone(),
            version: node.version.to_string(),
            level,
            installed_at,
            publisher,
            content_sha256: hash,
            signer_key_fp: None,
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
    let dir = safe_skill_dir(home, &manifest.name)?;
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
            ..Default::default()
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
pub(crate) fn caller_agent_name(home: &Path) -> Result<Option<String>> {
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

/// Drive `build()`'s future to completion on a dedicated thread with its own
/// current-thread runtime. Safe to call from inside an existing tokio runtime,
/// where `Handle::block_on` panics ("Cannot start a runtime from within a
/// runtime"). The future is built and driven entirely on the worker thread, so
/// it need not be `Send`; only the captured references cross the boundary.
fn block_on_isolated<B, F, T>(build: B) -> Result<T>
where
    B: FnOnce() -> F + Send,
    F: std::future::Future<Output = T>,
    T: Send,
{
    std::thread::scope(|s| {
        s.spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("build embed runtime")?;
            Ok(rt.block_on(build()))
        })
        .join()
        .map_err(|_| anyhow!("embed worker thread panicked"))?
    })
}

/// Best-effort embedding after install. Failure is non-fatal — the skill is
/// usable without an embedding (Jaccard path still works), and reindex-vec
/// can backfill.
fn try_embed_skill(home: &Path, name: &str, version: &str, manifest: &SkillManifest) {
    use crate::store::embedding::EmbeddingConfig;

    let cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
    let embed_config = EmbeddingConfig::from_config(&cfg);
    let index_dir = home.join("lance");

    // `mur` runs inside a tokio runtime, so `Handle::try_current()` succeeds
    // and `handle.block_on(...)` panics ("Cannot start a runtime from within a
    // runtime"). Drive the async embed on a dedicated thread with its own
    // runtime instead — safe whether or not a runtime is already active.
    let result = block_on_isolated(|| async {
        let store = crate::store::vector::factory::get_vector_store(&cfg, &index_dir).await?;
        crate::skill_index::embed_manifest_and_upsert(
            manifest,
            name,
            version,
            &embed_config,
            &*store,
        )
        .await
    })
    .and_then(|inner| inner);

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

    // Regression: install runs inside `mur`'s tokio runtime; the old
    // `Handle::try_current()` + `block_on` panicked ("Cannot start a runtime
    // from within a runtime"). `block_on_isolated` must complete instead.
    #[tokio::test]
    async fn block_on_isolated_runs_inside_ambient_runtime() {
        let out = block_on_isolated(|| async { 1 + 1 }).expect("no panic inside runtime");
        assert_eq!(out, 2);
    }

    fn node(name: &str, version: &str) -> ResolvedNode {
        let yaml = format!(
            "name: {name}\nversion: {version}\npublisher: human:t\ndescription: d\ncategory: context\ncontent:\n  abstract: a\n"
        );
        ResolvedNode {
            name: name.into(),
            version: semver::Version::parse(version).unwrap(),
            yaml_path: std::path::PathBuf::from("/nonexistent/skill.yaml"),
            manifest: mur_common::skill::parse_canonical(&yaml).unwrap(),
        }
    }

    /// #960 option 1: `mur skill install` must leave a drift baseline keyed by
    /// NAME, not only the hash-keyed allow-list entry.
    ///
    /// The hash-keyed entry cannot detect drift by construction — a changed
    /// skill simply lands under a different key, so there is nothing to compare
    /// against. Only an entry that survives the version change can answer "did
    /// this change since last time", which is what keying by name buys.
    #[test]
    fn install_writes_a_name_keyed_drift_baseline() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        let n = node("demo", "1.0.0");
        let registry = home.join("registry");
        std::fs::create_dir_all(&registry).unwrap();

        install_resolved_node(home, &registry, &n).unwrap();

        let trust = SkillTrustStore::load(home).unwrap();
        let hash = content_hash_for_trust(&n.manifest).unwrap();

        // Load-time allow-list entry, keyed by content hash.
        assert!(
            trust.entries.contains_key(&hash),
            "hash-keyed entry missing: {:?}",
            trust.entries.keys().collect::<Vec<_>>()
        );

        // Drift baseline, keyed by name, carrying a COMPARABLE content hash.
        let baseline = trust
            .entries
            .get("demo")
            .expect("no name-keyed drift baseline was written");
        assert_eq!(
            baseline.content_sha256, hash,
            "the baseline must pin the trust-domain hash, or it cannot be \
             compared against what registry-add pins"
        );
        assert_eq!(baseline.version, "1.0.0");
    }

    /// ...and the baseline is what makes a later content change detectable.
    /// Same name, same version, different content — previously invisible.
    #[test]
    fn the_baseline_makes_a_later_content_change_detectable() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        let registry = home.join("registry");
        std::fs::create_dir_all(&registry).unwrap();

        let first = node("demo", "1.0.0");
        install_resolved_node(home, &registry, &first).unwrap();
        let baseline = SkillTrustStore::load(home)
            .unwrap()
            .entries
            .get("demo")
            .unwrap()
            .content_sha256
            .clone();

        // The publisher rewrites the skill body without bumping the version.
        let mut second = node("demo", "1.0.0");
        second.manifest.description = "something else entirely".into();
        let new_hash = content_hash_for_trust(&second.manifest).unwrap();

        assert_ne!(
            baseline, new_hash,
            "a content rewrite must move the trust hash, or drift is undetectable"
        );
    }

    #[test]
    fn registry_install_stamps_origin() {
        let yaml = "name: t\nversion: 1.2.0\npublisher: human:mur-official\ndescription: d\ncategory: workflow\ncontent:\n  abstract: a\n";
        let mut m: SkillManifest = mur_common::skill::parse_canonical(yaml).unwrap();
        stamp_registry_origin(&mut m);
        assert_eq!(m.origin.as_deref(), Some("registry:human:mur-official/t"));
        assert_eq!(m.origin_version.as_deref(), Some("1.2.0"));
        assert_eq!(
            m.origin_hash.as_deref().unwrap(),
            mur_common::skill::hash::content_hash_for_origin(&m).unwrap()
        );
    }

    #[test]
    fn safe_skill_dir_rejects_traversal_names() {
        let home = tempdir().unwrap();
        // Separator/absolute forms (the dangerous traversal) are refused.
        for bad in ["../evil", "a/b", "../../etc/x", "a\\b", "/abs", ""] {
            assert!(
                safe_skill_dir(home.path(), bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
        // A normal name resolves under the skills dir.
        let dir = safe_skill_dir(home.path(), "web-search").unwrap();
        assert!(dir.starts_with(home.path().join("skills")));
    }

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
