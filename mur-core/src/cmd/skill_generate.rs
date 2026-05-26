//! `mur skill generate --from-session <id>` orchestrator.

use anyhow::{Context, Result, anyhow, bail};
use mur_common::config::Config;
use mur_common::error::LlmError;
use mur_common::llm::LlmClient;
use mur_common::skill::{
    SkillManifest, TrustLevel, content_sha256, global_skill_dir, scan::scan_skill,
    serialize_canonical, write_to_dir,
};
use mur_common::trust::skills::{SkillTrustStore, TrustEntry};
use std::path::Path;
use std::sync::Arc;

use crate::conversations::backend::ChatBackend;

pub struct GenerateOptions {
    pub session_id: String,
    pub name: Option<String>,
    pub model_override: Option<String>,
    pub dry_run: bool,
    pub max_parallel: usize,
}

/// Thin adapter: wraps a ChatBackend to satisfy the LlmClient trait.
struct ChatBackendAdapter {
    backend: Arc<dyn ChatBackend>,
    model: String,
}

impl LlmClient for ChatBackendAdapter {
    fn complete(
        &self,
        prompt: &str,
        system: Option<&str>,
    ) -> impl std::future::Future<Output = Result<String, LlmError>> + Send {
        let model = self.model.clone();
        let user = prompt.to_string();
        let sys = system.map(|s| s.to_string());
        let backend = self.backend.clone();
        async move {
            let req = crate::conversations::backend::ChatRequest {
                model: &model,
                system: sys.as_deref(),
                user: &user,
                max_tokens: 4096,
                temperature: Some(0.3),
                stop: vec![],
                cache_system: false,
                cache_user_prefix: None,
            };
            backend
                .generate(req)
                .await
                .map(|r| r.text)
                .map_err(|e| LlmError::Other(e.to_string()))
        }
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
        Ok(vec![])
    }
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
    let hash = content_sha256(&manifest)?;
    let mut trust = SkillTrustStore::load(home).map_err(|e| anyhow!("load trust: {e}"))?;
    trust.insert(
        hash,
        TrustEntry {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            level: TrustLevel::Sandboxed,
            installed_at: chrono::Utc::now().to_rfc3339(),
            publisher: Some(manifest.publisher.clone()),
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
            source: format!("agent:generator"),
        };
        if let Err(e) = crate::cross_agent::credit::ledger::append(home, &caller, &entry) {
            tracing::warn!("credit ledger append failed at generate: {e}");
        }
    }

    Ok(manifest)
}

pub async fn cmd_generate_cli(opts: GenerateOptions) -> Result<()> {
    let home = crate::cmd::agent::resolve_mur_home()?;
    let cfg = Config::load_or_default(&home.join("config.yaml"));
    let mut backend_cfg = cfg.conversations.ask.synthesize_backend();
    if let Some(ref model) = opts.model_override {
        backend_cfg.model = model.clone();
    }
    if !mur_common::llm::is_reasoning_model(&backend_cfg.model) {
        eprintln!(
            "warning: model '{}' is not a reasoning-class model — Error Analyst quality may suffer",
            backend_cfg.model
        );
    }
    let backend: Arc<dyn ChatBackend> =
        crate::conversations::backend::factory::build_for_stage(&backend_cfg, "skill.generate")
            .context("build llm")?;
    let llm = Arc::new(ChatBackendAdapter {
        backend,
        model: backend_cfg.model.clone(),
    });
    cmd_generate(&home, llm, opts).await?;
    Ok(())
}
