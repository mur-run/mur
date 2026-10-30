use anyhow::{Context, Result};
use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::*;
use std::io::{self, Write};

use crate::evolve;
use crate::retrieve::skill_candidates::LoadedSkill;
use crate::store::workflow_yaml::WorkflowYamlStore;

mod manage;
mod schedule;
mod suggest;

#[allow(unused_imports)]
pub(crate) use manage::*;
#[allow(unused_imports)]
pub(crate) use schedule::*;
pub use suggest::*;

/// Confirm before executing a workflow resolved by fuzzy match (semantic / keyword
/// / skill) rather than an exact name. Prevents a typo or near-miss query from
/// silently launching the wrong — possibly destructive — workflow. Non-interactive
/// callers must pass `--yes`; otherwise the run is refused (returns false).
fn confirm_fuzzy_run(
    query: &str,
    resolved: &str,
    via: &str,
    score: Option<f32>,
    yes: bool,
) -> bool {
    if yes {
        return true;
    }
    let score_str = score
        .map(|s| format!(", {:.0}% similar", s * 100.0))
        .unwrap_or_default();
    eprintln!(
        "⚠ No workflow named \"{query}\". Closest match (via {via}{score_str}): \"{resolved}\"."
    );
    if !std::io::IsTerminal::is_terminal(&io::stdin()) {
        eprintln!(
            "Refusing to auto-run a fuzzy-matched workflow non-interactively. \
             Re-run with the exact name, `--prompt` to inspect, or `--yes` to confirm."
        );
        return false;
    }
    eprint!("Run \"{resolved}\"? [y/N] ");
    let _ = io::stderr().flush();
    let mut line = String::new();
    let _ = io::stdin().read_line(&mut line);
    let ok = matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes");
    if !ok {
        eprintln!("Aborted.");
    }
    ok
}

/// Exact-name lookup for a workflow skill that can run unattended: `category:
/// Workflow` with a `content.procedure` to execute.
///
/// `workflow run` and `workflow schedule set` share this so a name that can be
/// scheduled is a name that will actually run. Deliberately exact-match only —
/// the fuzzy paths in `cmd_workflow_run` need a human to confirm, which a cron
/// or launchd fire has no way to obtain.
fn find_workflow_skill(mur_dir: &std::path::Path, name: &str) -> Option<LoadedSkill> {
    crate::retrieve::skill_candidates::load_skill_candidates(&mur_dir.join("skills"), mur_dir)
        .ok()?
        .into_iter()
        .find(|s| {
            s.manifest.name == name
                && s.manifest.category == mur_common::skill::types::Category::Workflow
                && s.manifest.content.procedure.is_some()
        })
}

/// Execute a workflow skill's DAG procedure.
async fn run_workflow_skill(
    mur_dir: &std::path::Path,
    skill: &LoadedSkill,
    yes: bool,
    channel: Option<String>,
    channel_new: bool,
) -> Result<()> {
    let Some(procedure) = &skill.manifest.content.procedure else {
        eprintln!(
            "Skill `{}` is category:Workflow but has no `content.procedure`",
            skill.manifest.name
        );
        return Ok(());
    };
    // Resolve channel_id: --channel-new creates a fresh channel,
    // --channel uses an existing one.
    let channel_id = if channel_new {
        let svc = mur_channel::ChannelService::open(mur_dir)?;
        let ch = svc.create_for_workflow(&skill.manifest.name)?;
        eprintln!("# channel: {}", ch.id);
        Some(ch.id)
    } else {
        channel
    };
    // #1607: gate `delegate_to` members before the first step (channel runs
    // only — without a channel those steps never dial anyone).
    let procedure = crate::executor::delegation::workflow::prepare_procedure(
        mur_dir,
        procedure,
        channel_id.as_deref(),
        &std::env::current_dir()?,
        crate::hitl::gate::GatePolicy {
            yes,
            unanswered: crate::executor::dag::default_unanswered(),
            auto_approve_tiers: vec![],
        },
    )
    .await?;
    let opts = crate::executor::dag::DagExecOptions {
        yes,
        device_id: "cli".to_string(),
        trigger: "manual",
        channel_id,
        run_id: format!("run-{}", uuid::Uuid::now_v7()),
        run_kind: Some(crate::run_status::RunKind::Workflow),
        run_label: skill.manifest.name.clone(),
        ..Default::default()
    };
    let output =
        crate::executor::dag::execute_dag(mur_dir, &skill.manifest.name, &procedure, &opts).await?;
    if output.exit_code != 0 {
        std::process::exit(output.exit_code);
    }
    Ok(())
}

/// Run a workflow — output as executable prompt for AI consumption.
/// Accepts exact name, semantic query, or pipeline expression (w1 | w2 && w3, w4).
pub(crate) async fn cmd_workflow_run(
    query: &str,
    fail_fast: bool,
    prompt: bool,
    yes: bool,
    channel: Option<String>,
    channel_new: bool,
) -> Result<()> {
    use crate::store::embedding::{EmbeddingConfig, embed};
    use crate::store::vector::LanceDbStore as VectorStore;
    use mur_common::pipeline::{has_pipeline_syntax, parse_pipeline_expr};

    // Detect pipeline syntax and delegate to PipelineExecutor
    if has_pipeline_syntax(query) {
        let expr = parse_pipeline_expr(query)
            .map_err(|e| anyhow::anyhow!("pipeline parse error: {}", e))?;
        let store = WorkflowYamlStore::default_store()?;
        let executor = crate::executor::pipeline::PipelineExecutor::new(store)
            .with_fail_fast(fail_fast)
            .with_yes(yes);
        let output = executor.execute(&expr, None).await?;
        if output.exit_code != 0 {
            eprintln!(
                "Pipeline finished with exit code {} ({})",
                output.exit_code, output.workflow_id
            );
        }
        return Ok(());
    }

    let store = WorkflowYamlStore::default_store()?;

    // Try exact match first
    if let Ok(w) = store.get(query) {
        if prompt {
            print_workflow_prompt(&w);
        } else {
            let executor = crate::executor::pipeline::PipelineExecutor::new(store)
                .with_fail_fast(fail_fast)
                .with_yes(yes);
            let expr = mur_common::pipeline::PipelineExpr::Single(w.name.clone());
            let output = executor.execute(&expr, None).await?;
            if output.exit_code != 0 {
                std::process::exit(output.exit_code);
            }
        }
        return Ok(());
    }

    // Exact workflow-skill match, BEFORE the fuzzy searches below. A scheduled
    // run has no TTY, so if semantic search claimed this name first the fuzzy
    // confirm would refuse it and the fire would be a silent no-op.
    let mur_dir = crate::paths::mur_root(None);
    if let Some(skill) = find_workflow_skill(&mur_dir, query) {
        if prompt {
            eprintln!("# {} — {}", skill.manifest.name, skill.manifest.description);
        } else {
            run_workflow_skill(&mur_dir, &skill, yes, channel, channel_new).await?;
        }
        return Ok(());
    }

    // Semantic search
    let index_path = crate::paths::mur_root(None).join("index");

    let mut best_name: Option<String> = None;
    // Track HOW we resolved a non-exact match, so we can confirm before executing.
    let mut match_via = "match";
    let mut match_score: Option<f32> = None;

    if index_path.exists() {
        let cfg = crate::store::config::load_config()?;
        let config = EmbeddingConfig::from_config(&cfg);
        if let Ok(query_embedding) = embed(query, &config).await {
            let vector_store =
                VectorStore::open(&index_path, cfg.embedding.dimensions as i32).await?;
            let results = vector_store
                .search(&query_embedding, 1, Some("workflow"))
                .await
                .map_err(|e| {
                    crate::store::vector::unreadable::hinted(
                        e,
                        crate::store::vector::unreadable::hint::PATTERNS,
                    )
                })?;
            if let Some(r) = results.first()
                && r.similarity > 0.6
            {
                best_name = Some(r.name.clone());
                match_via = "semantic search";
                match_score = Some(r.similarity);
            }
        }
    }

    // Fallback: keyword search
    if best_name.is_none() {
        let all = store.list_all()?;
        let q = query.to_lowercase();
        if let Some(w) = all.iter().find(|w| {
            let text = format!("{} {} {}", w.name, w.description, w.tools.join(" ")).to_lowercase();
            text.contains(&q)
        }) {
            best_name = Some(w.name.clone());
            match_via = "keyword match";
        }
    }

    match best_name {
        Some(name) => {
            if prompt {
                let w = store.get(&name)?;
                print_workflow_prompt(&w);
            } else if !confirm_fuzzy_run(query, &name, match_via, match_score, yes) {
                return Ok(());
            } else {
                let executor = crate::executor::pipeline::PipelineExecutor::new(store)
                    .with_fail_fast(fail_fast)
                    .with_yes(yes);
                let expr = mur_common::pipeline::PipelineExpr::Single(name);
                let output = executor.execute(&expr, None).await?;
                if output.exit_code != 0 {
                    std::process::exit(output.exit_code);
                }
            }
        }
        None => {
            // Fuzzy fallback: a category:Workflow skill whose name overlaps the
            // query. Exact matches already returned above.
            let candidates = crate::retrieve::skill_candidates::load_skill_candidates(
                &mur_dir.join("skills"),
                &mur_dir,
            )
            .unwrap_or_default();
            if let Some(matched) = candidates.iter().find(|s| {
                s.manifest.category == mur_common::skill::types::Category::Workflow
                    && (s.manifest.name.contains(query) || query.contains(&s.manifest.name))
            }) {
                if prompt {
                    eprintln!(
                        "# {} — {}",
                        matched.manifest.name, matched.manifest.description
                    );
                    return Ok(());
                }
                if !confirm_fuzzy_run(query, &matched.manifest.name, "skill match", None, yes) {
                    return Ok(());
                }
                run_workflow_skill(&mur_dir, matched, yes, channel, channel_new).await?;
                return Ok(());
            }

            // No skill match either.
            eprintln!("No matching workflow found for: {}", query);
            eprintln!("Available workflows:");
            let all = store.list_all()?;
            for w in &all {
                eprintln!("  {} — {}", w.name, w.description);
            }
        }
    }

    Ok(())
}

/// Print a workflow as an executable prompt for AI.
fn print_workflow_prompt(w: &mur_common::workflow::Workflow) {
    println!("# Workflow: {}\n", w.name);
    println!("{}\n", w.description);

    if !w.variables.is_empty() {
        println!("## Variables\n");
        for v in &w.variables {
            let req = if v.required { "required" } else { "optional" };
            let default = v.default.as_deref().unwrap_or("-");
            println!(
                "- `{}` ({}, {}): {} — default: `{}`",
                v.name,
                v.var_type,
                req,
                v.description.as_deref().unwrap_or(""),
                default
            );
        }
        println!();
    }

    if !w.tools.is_empty() {
        println!("## Tools\n");
        for t in &w.tools {
            println!("- {}", t);
        }
        println!();
    }

    if !w.steps.is_empty() {
        println!("## Steps\n");
        println!("Execute these steps in order:\n");
        for step in &w.steps {
            if let Some(cmd) = &step.command {
                println!("{}. {} (`{}`)", step.order, step.description, cmd);
            } else {
                println!("{}. {}", step.order, step.description);
            }
        }
        println!();
    }

    if !w.trigger.is_empty() {
        println!("## Trigger\n");
        println!("{}\n", w.trigger);
    }
}

#[cfg(test)]
mod tests;
