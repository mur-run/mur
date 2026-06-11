use anyhow::Result;

use crate::context_api;
use crate::inject;
use crate::store::workflow_yaml::WorkflowYamlStore;

// ─── Structured return types for MCP consumption ────────────────────
// Public API consumed by mur-mcp-server crate.

#[allow(dead_code)]
#[derive(Debug, Clone, serde::Serialize)]
pub struct ContextResult {
    pub patterns: Vec<ContextPattern>,
    pub project_context: Option<String>,
    pub token_count: usize,
}

#[allow(dead_code)]
#[derive(Debug, Clone, serde::Serialize)]
pub struct ContextPattern {
    pub name: String,
    pub description: String,
    pub content: String,
    pub tier: String,
}

/// Retrieve contextual skills for the current project. Returns structured
/// data suitable for MCP tool responses (no side effects, no printing).
/// (Pattern retrieval was removed in workflow-engine v2 P1b — skills and
/// workflows are the knowledge objects; the response shape is unchanged.)
#[allow(dead_code)]
pub async fn do_context(
    query: Option<String>,
    compact: bool,
    budget: usize,
) -> Result<ContextResult> {
    let cwd = std::env::current_dir()?;
    let project_name = cwd
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    // Build query from provided or auto-detected context
    let effective_query = match query {
        Some(q) => q,
        None => project_name.clone(),
    };

    let mur_dir = mur_common::trust::mur_home();
    let candidates =
        crate::retrieve::skill_candidates::load_skill_candidates(&mur_dir.join("skills"), &mur_dir)
            .unwrap_or_default();

    let context_patterns: Vec<ContextPattern> =
        crate::retrieve::scoring::score_and_rank_generic(&effective_query, candidates)
            .into_iter()
            .filter(|s| {
                s.item.stats.lifecycle_state != mur_common::skill::stats::LifecycleState::Archived
            })
            .map(|s| ContextPattern {
                name: s.item.manifest.name.clone(),
                description: s.item.manifest.description.clone(),
                content: s.item.manifest.description.clone(),
                tier: format!("{:?}", s.item.stats.lifecycle_state).to_lowercase(),
            })
            .collect();

    let token_budget = if compact { 800 } else { budget };
    let token_count = context_patterns.len() * 50; // rough estimate

    Ok(ContextResult {
        patterns: context_patterns,
        project_context: Some(project_name),
        token_count: token_count.min(token_budget),
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn cmd_context(
    query: Option<String>,
    compact: bool,
    write_file: bool,
    budget: usize,
    source: String,
    json_output: bool,
    scope_args: Vec<String>,
    quiet: bool,
) -> Result<()> {
    crate::auth::heartbeat();
    // Auto-pull from device sync if configured
    if !quiet
        && let Ok(config) = crate::store::config::load_config()
        && config.sync.auto
        && config.sync.method != "local"
        && let Err(e) =
            super::sync_cmd::device_sync(true, super::sync_cmd::DeviceSyncDirection::Pull, None)
                .await
    {
        eprintln!("  ⚠ Auto-pull failed: {}", e);
    }

    // Parse scope arguments (key=value pairs)
    let mut scope = context_api::ContextScope::default();
    for arg in &scope_args {
        if let Some((key, value)) = arg.split_once('=') {
            match key {
                "user" => scope.user = Some(value.to_string()),
                "project" => scope.project = Some(value.to_string()),
                "task" => scope.task = Some(value.to_string()),
                "platform" => scope.platform = Some(value.to_string()),
                _ => {
                    if !quiet {
                        eprintln!("Warning: unknown scope key '{}'", key);
                    }
                }
            }
        }
    }

    // Auto-detect project context from cwd
    let cwd = std::env::current_dir()?;

    let project_name = cwd
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    // Build query from provided or auto-detected context
    let effective_query = match query {
        Some(q) => q,
        None => {
            // Auto-detect: project name + recent git context
            let mut parts = vec![project_name.clone()];

            // Try to get git remote for additional context
            if let Ok(output) = std::process::Command::new("git")
                .args(["remote", "get-url", "origin"])
                .current_dir(&cwd)
                .output()
                && output.status.success()
            {
                let remote = String::from_utf8_lossy(&output.stdout).trim().to_string();
                // Extract repo name from URL
                if let Some(name) = remote.rsplit('/').next() {
                    let name = name.trim_end_matches(".git");
                    if name != project_name {
                        parts.push(name.to_string());
                    }
                }
            }

            // Try to get recent file context
            if let Ok(output) = std::process::Command::new("git")
                .args(["diff", "--name-only", "HEAD~3..HEAD"])
                .current_dir(&cwd)
                .output()
                && output.status.success()
            {
                let files = String::from_utf8_lossy(&output.stdout);
                for file in files.lines().take(5) {
                    // Extract keywords from file paths
                    let stem = std::path::Path::new(file)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("");
                    if !stem.is_empty() && stem.len() > 2 {
                        parts.push(stem.to_string());
                    }
                }
            }

            parts.join(" ")
        }
    };

    let workflow_store = WorkflowYamlStore::default_store()?;
    let workflows = workflow_store.list_all()?;

    // Skills are the knowledge objects (workflow-engine v2 P1b); `scope` is
    // still parsed for CLI compatibility but skills carry no origin scopes.
    let _ = (&scope, &source);
    let mur_dir = mur_common::trust::mur_home();
    let candidates =
        crate::retrieve::skill_candidates::load_skill_candidates(&mur_dir.join("skills"), &mur_dir)
            .unwrap_or_default();
    let scored: Vec<_> =
        crate::retrieve::scoring::score_and_rank_generic(&effective_query, candidates)
            .into_iter()
            .filter(|s| {
                s.item.stats.lifecycle_state != mur_common::skill::stats::LifecycleState::Archived
            })
            .collect();

    let token_budget = if compact { 800 } else { budget };

    // If JSON output requested, build ContextResponse from the scored skills
    // (same pipeline as the text path).
    if json_output {
        use context_api::{ContextResponse, ScoredPatternResponse};
        let response_patterns: Vec<ScoredPatternResponse> = scored
            .iter()
            .map(|s| ScoredPatternResponse {
                name: s.item.manifest.name.clone(),
                description: s.item.manifest.description.clone(),
                score: s.score,
                kind: format!("{:?}", s.item.manifest.category).to_lowercase(),
            })
            .collect();
        let formatted =
            inject::hook::format_skills_for_injection(&scored, &workflows, token_budget);
        let resp = ContextResponse {
            tokens_used: formatted.len() / 4,
            injection_ids: scored
                .iter()
                .map(|s| s.item.manifest.name.clone())
                .collect(),
            patterns: response_patterns,
            formatted,
        };
        let json = serde_json::to_string_pretty(&resp)?;
        println!("{json}");
        return Ok(());
    }

    let output = inject::hook::format_skills_for_injection(&scored, &workflows, token_budget);

    if !output.is_empty() {
        if write_file {
            // Write to ~/.mur/context.md for file-based tools
            let context_path = crate::paths::mur_root(None).join("context.md");
            let file_content = format!(
                "# MUR Context (auto-generated)\n# Query: {}\n# Updated: {}\n\n{}\n",
                effective_query,
                chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
                output
            );
            std::fs::write(&context_path, file_content)?;
            if !quiet {
                eprintln!("📝 Context written to {}", context_path.display());
            }
        } else {
            print!("{}", output);
        }
    }

    Ok(())
}
