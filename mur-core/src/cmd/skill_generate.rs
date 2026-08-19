//! `mur skill generate --from-session <id>` orchestrator.

use anyhow::{Context, Result, anyhow, bail};
use mur_common::llm::LlmClient;
use mur_common::skill::{
    SkillManifest, TrustLevel, content_hash_for_trust, global_skill_dir, scan::scan_skill,
    serialize_canonical, write_to_dir,
};
use mur_common::trust::skills::{SkillTrustStore, TrustEntry};
use std::path::Path;
use std::sync::Arc;

use crate::conversations::backend::adapter::build_chat_adapter;

pub struct GenerateOptions {
    pub session_id: String,
    pub name: Option<String>,
    pub model_override: Option<String>,
    pub dry_run: bool,
    pub max_parallel: usize,
}

pub async fn cmd_generate<L: LlmClient + 'static>(
    home: &Path,
    llm: Arc<L>,
    opts: GenerateOptions,
) -> Result<SkillManifest> {
    let path = home
        .join("session/recordings")
        .join(format!("{}.jsonl", opts.session_id));
    if !path.exists() {
        bail!("no recording at {}", path.display());
    }
    let content =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;

    let trajectories =
        crate::skill_gen::trajectory::parse_recording(&content).context("parse recording")?;
    if trajectories.is_empty() {
        bail!("recording produced zero trajectories");
    }
    eprintln!("Phase 1: {} trajectories", trajectories.len());

    let patch_results =
        crate::skill_gen::analysts::run_phase2(llm.clone(), trajectories, opts.max_parallel).await;
    let mut patches = Vec::new();
    let mut analyst_failures = 0;
    for r in patch_results {
        match r {
            Ok(p) => patches.push(p),
            Err(e) => {
                analyst_failures += 1;
                tracing::warn!(error = %e, "analyst failure (dropped)");
            }
        }
    }
    if patches.is_empty() {
        bail!("all analysts failed ({analyst_failures} failures)");
    }
    eprintln!(
        "Phase 2: {} patches ({} failures)",
        patches.len(),
        analyst_failures
    );

    let manifest =
        crate::skill_gen::consolidator::consolidate(patches, &*llm, opts.name.as_deref())
            .await
            .context("consolidate")?;
    eprintln!("Phase 3: '{}' v{}", manifest.name, manifest.version);

    if opts.dry_run {
        let yaml = serialize_canonical(&manifest)?;
        println!("{yaml}");
        return Ok(manifest);
    }

    // Write + scan + trust (agent-generated == Sandboxed per spec §7.5).
    let dir = global_skill_dir(home, &manifest.name);
    write_to_dir(&dir, &manifest)?;
    let report = scan_skill(&manifest)?;
    if report.has_blocking_findings() {
        eprintln!("⚠ scan findings — Sandboxed trust:");
        for line in report.human_summary() {
            eprintln!("    {line}");
        }
    }
    // Trust-store key: the trust hash (see `content_hash_for_trust`).
    let hash = content_hash_for_trust(&manifest)?;
    let mut trust = SkillTrustStore::load(home).map_err(|e| anyhow!("load trust: {e}"))?;
    trust.insert(
        hash,
        TrustEntry {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            level: TrustLevel::Sandboxed,
            installed_at: chrono::Utc::now().to_rfc3339(),
            publisher: Some(manifest.publisher.clone()),
            ..Default::default()
        },
    );
    trust.save(home).map_err(|e| anyhow!("save trust: {e}"))?;
    println!(
        "generated: {} v{} (Sandboxed)",
        manifest.name, manifest.version
    );
    println!("review:    {}", dir.join("skill.yaml").display());

    // M7c: append Author entry to credit ledger.
    if let Ok(Some(caller)) = crate::cmd::skill_install::caller_agent_name(home) {
        let entry = mur_common::skill::credit::CreditEntry {
            ts: chrono::Utc::now(),
            skill: manifest.name.clone(),
            skill_version: manifest.version.clone(),
            kind: mur_common::skill::credit::CreditKind::Author,
            agent: caller.clone(),
            evidence: None,
            source: "agent:generator".to_string(),
        };
        if let Err(e) = crate::cross_agent::credit::ledger::append(home, &caller, &entry) {
            tracing::warn!("credit ledger append failed at generate: {e}");
        }
    }

    Ok(manifest)
}

pub async fn cmd_generate_cli(opts: GenerateOptions) -> Result<()> {
    let home = crate::cmd::agent::resolve_mur_home()?;
    let adapter = build_chat_adapter(&home, opts.model_override.as_deref(), "skill.generate")
        .context("build llm")?;
    if !mur_common::llm::is_reasoning_model(&adapter.model) {
        eprintln!(
            "warning: model '{}' is not a reasoning-class model — Error Analyst quality may suffer",
            adapter.model
        );
    }
    let llm = Arc::new(adapter);
    cmd_generate(&home, llm, opts).await?;
    Ok(())
}
