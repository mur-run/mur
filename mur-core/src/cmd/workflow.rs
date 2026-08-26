use anyhow::{Context, Result};
use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::*;
use std::io::{self, Write};

use crate::evolve;
use crate::retrieve::skill_candidates::LoadedSkill;
use crate::store::workflow_yaml::WorkflowYamlStore;

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
        crate::executor::dag::execute_dag(mur_dir, &skill.manifest.name, procedure, &opts).await?;
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
                .await?;
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

pub(crate) fn cmd_workflow_list() -> Result<()> {
    let store = WorkflowYamlStore::default_store()?;
    let workflows = store.list_all()?;

    if workflows.is_empty() {
        println!("No workflows found. Create one with `mur workflow new`.");
        return Ok(());
    }

    println!("📋 Workflows ({}):\n", workflows.len());
    for w in &workflows {
        let steps = w.steps.len();
        println!("  {} — {} ({} steps)", w.name, w.description, steps);
    }

    Ok(())
}

pub(crate) fn cmd_workflow_show(name: &str, markdown: bool) -> Result<()> {
    let store = WorkflowYamlStore::default_store()?;
    let w = store.get(name)?;

    if markdown {
        // Markdown output optimized for AI consumption
        println!("# {}\n", w.name);
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
    } else {
        // Human-readable output
        println!("📋 Workflow: {}\n", w.name);
        println!("Description: {}", w.description);

        let content_text = w.content.as_text();
        if !content_text.is_empty() {
            println!("Content: {}", content_text);
        }

        if !w.variables.is_empty() {
            println!("\nVariables:");
            for v in &w.variables {
                let req = if v.required { "required" } else { "optional" };
                let default = v.default.as_deref().unwrap_or("-");
                println!(
                    "  ${} ({}): {} [{}] default={}",
                    v.name,
                    v.var_type,
                    v.description.as_deref().unwrap_or(""),
                    req,
                    default
                );
            }
        }

        if !w.steps.is_empty() {
            println!("\nSteps:");
            for step in &w.steps {
                print!("  {}. {}", step.order, step.description);
                if let Some(cmd) = &step.command {
                    print!(" (`{}`)", cmd);
                }
                println!();
            }
        }

        if !w.tools.is_empty() {
            println!("\nTools: {}", w.tools.join(", "));
        }

        if !w.trigger.is_empty() {
            println!("Trigger: {}", w.trigger);
        }
    }

    Ok(())
}

/// Semantic search for workflows using LanceDB embeddings.
pub(crate) async fn cmd_workflow_search(query: &str, limit: usize) -> Result<()> {
    use crate::store::embedding::{EmbeddingConfig, embed};
    use crate::store::vector::LanceDbStore as VectorStore;

    let store = WorkflowYamlStore::default_store()?;
    let all_workflows = store.list_all()?;

    if all_workflows.is_empty() {
        println!(
            "No workflows found. Create one with `mur workflow new` or extract from a session."
        );
        return Ok(());
    }

    let index_path = crate::paths::mur_root(None).join("index");

    if index_path.exists() {
        // Semantic search via LanceDB
        let cfg = crate::store::config::load_config()?;
        let config = EmbeddingConfig::from_config(&cfg);
        match embed(query, &config).await {
            Ok(query_embedding) => {
                let vector_store =
                    VectorStore::open(&index_path, cfg.embedding.dimensions as i32).await?;
                // Search with item_type filter = "workflow"
                let results = vector_store
                    .search(&query_embedding, limit, Some("workflow"))
                    .await?;

                if results.is_empty() {
                    println!("No matching workflows found for: {}", query);
                    return Ok(());
                }

                println!("🔍 Workflow search: \"{}\"\n", query);
                for (i, r) in results.iter().enumerate() {
                    // Find the full workflow to show details
                    if let Some(w) = all_workflows.iter().find(|w| w.name == r.name) {
                        let steps = w.steps.len();
                        let tools = if w.tools.is_empty() {
                            String::new()
                        } else {
                            format!(" [{}]", w.tools.join(", "))
                        };
                        let score = (r.similarity * 100.0) as u32;
                        println!(
                            "  {}. {} ({}% match, {} steps){}",
                            i + 1,
                            w.name,
                            score,
                            steps,
                            tools
                        );
                        println!("     {}", w.description);
                    } else {
                        println!(
                            "  {}. {} ({:.0}% match)",
                            i + 1,
                            r.name,
                            r.similarity * 100.0
                        );
                    }
                }
                println!("\nUse `mur workflow show <name>` for full details.");
                return Ok(());
            }
            Err(e) => {
                eprintln!(
                    "⚠ Embedding unavailable ({}), falling back to keyword search",
                    e
                );
            }
        }
    }

    // Fallback: keyword search
    let query_lower = query.to_lowercase();
    let matches: Vec<_> = all_workflows
        .iter()
        .filter(|w| {
            let text = format!("{} {} {}", w.name, w.description, w.tools.join(" ")).to_lowercase();
            text.contains(&query_lower)
        })
        .collect();

    if matches.is_empty() {
        println!("No matching workflows found for: {}", query);
        return Ok(());
    }

    println!(
        "🔍 Workflow search: \"{}\" ({} results)\n",
        query,
        matches.len()
    );
    for (i, w) in matches.iter().enumerate() {
        let tools = if w.tools.is_empty() {
            String::new()
        } else {
            format!(" [{}]", w.tools.join(", "))
        };
        println!("  {}. {} ({} steps){}", i + 1, w.name, w.steps.len(), tools);
        println!("     {}", w.description);
    }
    println!("\nUse `mur workflow show <name>` for full details.");
    Ok(())
}

pub(crate) fn cmd_workflow_new() -> Result<()> {
    use mur_common::workflow::Step;

    let store = WorkflowYamlStore::default_store()?;

    print!("Workflow name (kebab-case): ");
    io::stdout().flush()?;
    let mut name = String::new();
    io::stdin().read_line(&mut name)?;
    let name = name.trim().to_string();

    if name.is_empty() {
        println!("Name cannot be empty.");
        return Ok(());
    }
    if store.exists(&name) {
        println!("Workflow '{}' already exists.", name);
        return Ok(());
    }

    print!("Description: ");
    io::stdout().flush()?;
    let mut desc = String::new();
    io::stdin().read_line(&mut desc)?;
    let desc = desc.trim().to_string();

    print!("Trigger (when to use, e.g. 'when deploying to production'): ");
    io::stdout().flush()?;
    let mut trigger = String::new();
    io::stdin().read_line(&mut trigger)?;
    let trigger = trigger.trim().to_string();

    println!("Steps (enter description, empty line to finish):");
    let mut steps = Vec::new();
    let mut order = 1u32;
    loop {
        print!("  Step {}: ", order);
        io::stdout().flush()?;
        let mut step_desc = String::new();
        io::stdin().read_line(&mut step_desc)?;
        let step_desc = step_desc.trim().to_string();
        if step_desc.is_empty() {
            break;
        }

        print!("    Command (optional): ");
        io::stdout().flush()?;
        let mut cmd = String::new();
        io::stdin().read_line(&mut cmd)?;
        let cmd = cmd.trim().to_string();

        steps.push(Step {
            order,
            description: step_desc,
            command: if cmd.is_empty() { None } else { Some(cmd) },
            ..Default::default()
        });
        order += 1;
    }

    let workflow = mur_common::workflow::Workflow {
        base: KnowledgeBase {
            name: name.clone(),
            description: desc,
            content: Content::Plain(trigger.clone()),
            ..Default::default()
        },
        steps,
        variables: vec![],
        source_sessions: vec![],
        trigger,
        tools: vec![],
        published_version: 0,
        permission: Default::default(),
        schedule: None,
        id: None,
        notify: None,
        requires: vec![],
    };

    store.save(&workflow)?;
    println!("Created workflow: {}", name);
    Ok(())
}

/// Publish a workflow to a team.
/// POST {server_url}/api/v1/core/workflows/{name}/publish with team_slug.
pub(crate) fn cmd_workflow_publish(name: &str, team: &str) -> Result<()> {
    let store = WorkflowYamlStore::default_store()?;
    let workflow = store.get(name)?;

    let server_url = crate::auth::server_url();
    let token = match crate::auth::load_tokens() {
        Some(t) => t.access_token,
        None => {
            eprintln!("Not logged in. Run `mur auth login` first.");
            return Ok(());
        }
    };

    // License check — warn but don't block (server enforces 403)
    eprintln!(
        "  ⚠ Publishing requires a Pro+ plan. If you're on Free, the server will reject this."
    );

    let device_id = crate::auth::get_device_id();
    let device_name = crate::auth::get_device_name();
    let device_os = crate::auth::get_device_os();

    let yaml_content = std::fs::read_to_string(
        crate::paths::mur_root(None)
            .join("workflows")
            .join(format!("{}.yaml", name)),
    )?;

    let payload = serde_json::json!({
        "name": workflow.name,
        "yaml_content": yaml_content,
        "team_slug": team,
    });
    let body = serde_json::to_string(&payload)?;
    let url = format!(
        "{}/api/v1/core/workflows/{}/publish",
        server_url,
        urlencoding::encode(name)
    );

    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(&url)
        .timeout(std::time::Duration::from_secs(15))
        .header("Authorization", format!("Bearer {}", token))
        .header("X-Device-ID", device_id)
        .header("X-Device-Name", device_name)
        .header("X-Device-OS", device_os)
        .header("Content-Type", "application/json")
        .body(body)
        .send();

    match resp {
        Ok(r) if r.status().is_success() => {
            println!("✓ Published workflow '{}' to team '{}'.", name, team);
        }
        Ok(r) => {
            let status = r.status();
            let body = r.text().unwrap_or_default();
            eprintln!(
                "✗ Publish failed: HTTP {}{}",
                status,
                if !body.trim().is_empty() {
                    format!(" ({})", body.trim())
                } else {
                    String::new()
                }
            );
        }
        Err(e) => {
            eprintln!("✗ Publish failed: {}", e);
        }
    }

    Ok(())
}

/// Install a workflow from a team.
/// GET team workflows, download YAML to ~/.mur/workflows/.
pub(crate) fn cmd_workflow_install(name: &str, team: &str) -> Result<()> {
    let server_url = crate::auth::server_url();
    let token = match crate::auth::load_tokens() {
        Some(t) => t.access_token,
        None => {
            eprintln!("Not logged in. Run `mur auth login` first.");
            return Ok(());
        }
    };

    let device_id = crate::auth::get_device_id();
    let device_name = crate::auth::get_device_name();
    let device_os = crate::auth::get_device_os();

    let url = format!(
        "{}/api/v1/core/teams/{}/workflows/{}",
        server_url,
        urlencoding::encode(team),
        urlencoding::encode(name),
    );

    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(15))
        .header("Authorization", format!("Bearer {}", token))
        .header("X-Device-ID", device_id)
        .header("X-Device-Name", device_name)
        .header("X-Device-OS", device_os)
        .send();

    match resp {
        Ok(r) if r.status().is_success() => {
            let body = r.text().unwrap_or_default();

            // Response should contain the workflow YAML content
            let resp: serde_json::Value = serde_json::from_str(&body)?;
            let yaml_content = resp
                .get("yaml_content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("Server response missing yaml_content field"))?;

            // Write to ~/.mur/workflows/
            let store = WorkflowYamlStore::default_store()?;
            let workflow: mur_common::workflow::Workflow = serde_yaml::from_str(yaml_content)?;
            store.save(&workflow)?;

            println!(
                "✓ Installed workflow '{}' from team '{}' ({} steps).",
                workflow.name,
                team,
                workflow.steps.len()
            );
        }
        Ok(r) => {
            eprintln!(
                "✗ Install failed: {}",
                if r.status().as_u16() == 404 {
                    "workflow not found or access denied".to_string()
                } else {
                    format!("HTTP {}", r.status())
                }
            );
        }
        Err(e) => {
            eprintln!("✗ Install failed: {}", e);
        }
    }

    Ok(())
}

pub(crate) fn cmd_suggest(create: bool, accept: Option<&str>, dismiss: Option<&str>) -> Result<()> {
    // ─── Nudge accept/dismiss (early exit) ────────────────────────────
    if let Some(id) = accept {
        return cmd_suggest_accept(id);
    }
    if let Some(id) = dismiss {
        return cmd_suggest_dismiss(id);
    }

    // Drain companion nudge responses before listing pending.
    let _ = crate::nudge::companion::drain_nudge_responses_in(
        &crate::store::yaml::default_mur_dir(),
        &|c| create_draft_workflow(&c.suggested_name, &c.title, "", &c.evidence_session_ids),
    );

    use evolve::cooccurrence::CooccurrenceMatrix;

    let workflow_store = WorkflowYamlStore::default_store()?;
    let workflows = workflow_store.list_all()?;

    // ─── Part 1: Workflow composition from co-occurrence ─────────────

    let matrix_path = CooccurrenceMatrix::default_path();
    let matrix = CooccurrenceMatrix::load(&matrix_path)?;

    println!("🔗 Knowledge ↔ Workflow Intelligence\n");
    println!("── Co-occurrence Data ──");
    println!("  Tracked pairs: {}", matrix.pair_count());

    // Part 1: Co-occurrence data shown above (composition suggestions removed with Pattern system)

    // ─── Part 2 (removed): Workflow decomposition into patterns ──────────────
    // Pattern decomposition removed — skills use skill/lifecycle.rs evolve path.
    let _ = (&workflows, create);

    // ─── Part 3: Pending nudges ───────────────────────────────────────

    let nudge_path = crate::nudge::NudgeLedger::default_path();
    if let Ok(ledger) = crate::nudge::NudgeLedger::load(&nudge_path) {
        let now = chrono::Utc::now();
        let mut pending: Vec<(&String, &crate::nudge::NudgeRecord, &str)> = Vec::new();
        for (id, rec) in &ledger.records {
            let status = match &rec.state {
                crate::nudge::NudgeState::Surfaced => "surfaced",
                crate::nudge::NudgeState::Snoozed { until } => {
                    let expired = chrono::DateTime::parse_from_rfc3339(until)
                        .map(|u| now >= u.with_timezone(&chrono::Utc))
                        .unwrap_or(true);
                    if expired {
                        "snoozed-expired"
                    } else {
                        continue;
                    }
                }
                _ => continue,
            };
            pending.push((id, rec, status));
        }

        if !pending.is_empty() {
            println!("── Pending Workflow Nudges ──\n");
            for (id, rec, status) in &pending {
                let short_id = if id.len() > 8 { &id[..8] } else { id };
                let title = rec
                    .candidate
                    .as_ref()
                    .map(|c| c.title.as_str())
                    .unwrap_or("(unknown)");
                let sessions = rec.candidate.as_ref().map(|c| c.session_count).unwrap_or(0);
                println!(
                    "  [{}] {} (seen in {} sessions) [{}]",
                    short_id, title, sessions, status
                );
            }
            println!();
            println!("  Accept:  mur suggest --accept <id>");
            println!("  Dismiss: mur suggest --dismiss <id>");
            println!();
        }
    }

    // ─── Summary ─────────────────────────────────────────────────────

    if !create && !workflows.is_empty() {
        println!("Run `mur suggest --create` to auto-create suggested items as drafts.");
    }

    Ok(())
}

pub(crate) fn cmd_suggest_accept(id: &str) -> Result<()> {
    let config_path = crate::store::yaml::default_mur_dir().join("config.yaml");
    let cfg = mur_common::config::Config::load_or_default(&config_path);
    let path = crate::nudge::NudgeLedger::default_path();
    let mut ledger = crate::nudge::NudgeLedger::load(&path)?;
    crate::nudge::NudgeEmitter::apply_decision(
        &mut ledger,
        id,
        crate::nudge::NudgeDecision::Accept,
        cfg.nudge.snooze_days,
        chrono::Utc::now(),
        &|c| create_draft_workflow(&c.suggested_name, &c.title, "", &c.evidence_session_ids),
    )?;
    ledger.save(&path)?;
    println!(
        "✓ Saved workflow draft from nudge {}. Run it with `mur run <name>`.",
        id
    );
    Ok(())
}

pub(crate) fn cmd_suggest_dismiss(id: &str) -> Result<()> {
    let config_path = crate::store::yaml::default_mur_dir().join("config.yaml");
    let cfg = mur_common::config::Config::load_or_default(&config_path);
    let path = crate::nudge::NudgeLedger::default_path();
    let mut ledger = crate::nudge::NudgeLedger::load(&path)?;
    crate::nudge::NudgeEmitter::apply_decision(
        &mut ledger,
        id,
        crate::nudge::NudgeDecision::Dismiss,
        cfg.nudge.snooze_days,
        chrono::Utc::now(),
        &|_| Ok(()),
    )?;
    ledger.save(&path)?;
    println!("✓ Dismissed nudge {}.", id);
    Ok(())
}

// ─── Schedule management (uses ~/.mur/schedules.yaml) ──────────────────────

use mur_common::schedule::{Schedule, ScheduleNotify, SchedulesFile};

fn schedules_path() -> std::path::PathBuf {
    crate::paths::mur_root(None).join("schedules.yaml")
}

fn load_schedules() -> Result<Vec<Schedule>> {
    let path = schedules_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&path)?;
    let file: SchedulesFile = serde_yaml::from_str(&content)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(file.schedules)
}

fn save_schedules(schedules: &[Schedule]) -> Result<()> {
    let path = schedules_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = SchedulesFile {
        schedules: schedules.to_vec(),
    };
    let yaml = serde_yaml::to_string(&file)?;
    std::fs::write(&path, yaml)?;
    Ok(())
}

pub(crate) fn cmd_schedule_list() -> Result<()> {
    let schedules = load_schedules()?;

    if schedules.is_empty() {
        println!("📋 No scheduled workflows.");
        println!("  Use `mur workflow schedule set <name> \"0 * * * *\"` to add one.");
        return Ok(());
    }

    let active: Vec<_> = schedules.iter().filter(|s| s.enabled).collect();
    let disabled: Vec<_> = schedules.iter().filter(|s| !s.enabled).collect();

    if !active.is_empty() {
        println!("🔄 Active schedules:\n");
        // Show the zone the schedule will ACTUALLY fire in, not the stored
        // label: launchd and crontab both read the cron expression as local
        // time regardless of what `timezone` says. Entries written before this
        // was fixed all carry a hardcoded "UTC" that never applied.
        let local_tz = iana_time_zone::get_timezone().unwrap_or_else(|_| "local".to_string());
        for s in &active {
            println!("  {} — `{}` ({})", s.workflow, s.cron, local_tz);
            if s.timezone != local_tz {
                println!("     stored timezone: {} (not used locally)", s.timezone);
            }
            if !s.user_id.is_empty() {
                println!("     user: {}", s.user_id);
            }
            if !s.notify.target.is_empty() {
                println!(
                    "     notify: {} → {}",
                    s.notify.notify_type, s.notify.target
                );
            }
        }
    }

    if !disabled.is_empty() {
        println!("\n⏸️  Disabled:\n");
        for s in &disabled {
            println!("  {} — `{}`", s.workflow, s.cron);
        }
    }

    println!("\n  Total: {} schedule(s)", schedules.len());

    // Show system-level schedule status
    let system = crate::cmd::system_schedule::list_system_schedules();
    if !system.is_empty() {
        println!("\n🖥️  System schedules:");
        for (name, backend) in &system {
            println!("  {} ({})", name, backend);
        }
    }
    Ok(())
}

pub(crate) fn cmd_schedule_set(name: &str, cron: &str) -> Result<()> {
    // Validate cron
    let parts: Vec<&str> = cron.split_whitespace().collect();
    if parts.len() < 5 || parts.len() > 7 {
        anyhow::bail!(
            "Invalid cron expression '{}'. Expected 5-7 fields (e.g. '0 * * * *' for hourly).",
            cron
        );
    }

    // Check for stale Commander PID — if PID file exists but process is dead, reclaim for system cron
    if mur_common::schedule_claim::commander_pid_path().exists()
        && !mur_common::schedule_claim::is_commander_running()
        && let Ok(released) = mur_common::schedule_claim::release_all_from_commander()
        && !released.is_empty()
    {
        eprintln!(
            "   ⚠️  Commander not running (stale PID). Reclaiming {} schedule(s) for system cron.",
            released.len()
        );
    }

    // Verify the name resolves to something the scheduler can actually fire.
    // A schedule runs `mur run <name>`, which resolves flat workflows AND
    // exact-named workflow skills — so both are valid here.
    let store = WorkflowYamlStore::default_store()?;
    if store.get(name).is_err()
        && find_workflow_skill(&crate::paths::mur_root(None), name).is_none()
    {
        anyhow::bail!(
            "Workflow '{}' not found — no ~/.mur/workflows/{}.yaml, and no `category: Workflow` \
             skill of that name with a `content.procedure`.",
            name,
            name
        );
    }

    // Determine executor: Commander if running, otherwise system cron
    let executor = mur_common::schedule_claim::auto_detect_executor();

    let mut schedules = load_schedules()?;

    // Both executors interpret the cron expression in LOCAL time: launchd's
    // StartCalendarInterval and crontab both do. Record the real IANA zone —
    // labelling every schedule "UTC" told users their `20 23 * * *` fired at
    // 23:20 UTC when it actually fires at 23:20 local, an 8-hour lie in +08:00.
    let timezone = iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_string());

    // Update existing or add new
    if let Some(existing) = schedules.iter_mut().find(|s| s.workflow == name) {
        existing.cron = cron.to_string();
        existing.enabled = true;
        existing.executor = executor.clone();
        existing.timezone = timezone;
    } else {
        schedules.push(Schedule {
            id: uuid::Uuid::new_v4().to_string(),
            user_id: String::new(),
            workflow: name.to_string(),
            cron: cron.to_string(),
            timezone,
            enabled: true,
            notify: ScheduleNotify::default(),
            variables: Default::default(),
            on_missed: Default::default(),
            executor: executor.clone(),
        });
    }

    save_schedules(&schedules)?;
    println!("✅ Schedule set for '{}': {}", name, cron);

    // Only install system cron if Commander isn't handling it
    if executor == mur_common::schedule::ScheduleExecutor::SystemCron {
        // Install system-level scheduler (launchd on macOS, crontab on Linux)
        match crate::cmd::system_schedule::install(name, cron) {
            Ok(()) => {
                if cfg!(target_os = "macos") {
                    println!(
                        "   launchd plist installed at ~/Library/LaunchAgents/com.mur.schedule.{}.plist",
                        name
                    );
                } else {
                    println!("   crontab entry installed");
                }
            }
            Err(e) => {
                eprintln!(
                    "   ⚠️  Failed to install system schedule: {}. Workflow will only run when Commander is active.",
                    e
                );
            }
        }
    } else {
        println!("   Commander daemon is running — it will handle this schedule.");
    }
    Ok(())
}

pub(crate) fn cmd_schedule_remove(name: &str) -> Result<()> {
    let mut schedules = load_schedules()?;
    let before = schedules.len();
    schedules.retain(|s| s.workflow != name);

    if schedules.len() == before {
        println!("ℹ️  No schedule found for '{}'.", name);
        return Ok(());
    }

    save_schedules(&schedules)?;
    println!("🗑️  Schedule removed for '{}'.", name);

    // Remove system-level scheduler
    if let Err(e) = crate::cmd::system_schedule::remove(name) {
        eprintln!("   ⚠️  Failed to remove system schedule: {}", e);
    }
    Ok(())
}

pub(crate) fn cmd_schedule_enable(name: &str, enable: bool) -> Result<()> {
    let mut schedules = load_schedules()?;

    if let Some(entry) = schedules.iter_mut().find(|s| s.workflow == name) {
        entry.enabled = enable;
        save_schedules(&schedules)?;
        if enable {
            println!("▶️  Schedule enabled for '{}'.", name);
            if let Some(s) = schedules.iter().find(|s| s.workflow == name) {
                let _ = crate::cmd::system_schedule::install(name, &s.cron);
            }
        } else {
            println!("⏸️  Schedule disabled for '{}'.", name);
            let _ = crate::cmd::system_schedule::remove(name);
        }
    } else {
        println!(
            "ℹ️  No schedule found for '{}'. Use `mur workflow schedule set` first.",
            name
        );
    }

    Ok(())
}

/// Build and save a draft Workflow. Shared by `mur suggest --create` and nudge-accept.
pub fn create_draft_workflow_in(
    store: &crate::store::workflow_yaml::WorkflowYamlStore,
    name: &str,
    description: &str,
    trigger: &str,
    source_sessions: &[String],
) -> anyhow::Result<()> {
    if store.exists(name) {
        return Ok(()); // idempotent: don't clobber an existing workflow
    }
    let base = KnowledgeBase {
        name: name.to_string(),
        description: description.to_string(),
        content: Content::Plain(trigger.to_string()),
        maturity: mur_common::knowledge::Maturity::Draft,
        ..Default::default()
    };
    let wf = mur_common::workflow::Workflow {
        base,
        steps: vec![],
        variables: vec![],
        source_sessions: source_sessions.to_vec(),
        trigger: trigger.to_string(),
        tools: vec![],
        published_version: 0,
        permission: Default::default(),
        schedule: None,
        id: None,
        notify: None,
        requires: vec![],
    };
    store.save(&wf)
}

/// Convenience over the default store.
pub fn create_draft_workflow(
    name: &str,
    description: &str,
    trigger: &str,
    source_sessions: &[String],
) -> anyhow::Result<()> {
    let store = crate::store::workflow_yaml::WorkflowYamlStore::default_store()?;
    create_draft_workflow_in(&store, name, description, trigger, source_sessions)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest flat workflow YAML that loads. Everything else in
    /// `KnowledgeBase` carries a serde default — including `schema`, which
    /// fills in as 3. Pinned as a test because hand-writing this file is
    /// otherwise a guessing game against a `#[serde(flatten)]` struct.
    #[test]
    fn minimal_flat_workflow_yaml_loads() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("workflows");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("probe.yaml"),
            "name: probe\n\
             description: two shell steps\n\
             content: what this workflow is for\n\
             steps:\n\
             - order: 1\n  description: first\n  command: echo one\n\
             - order: 2\n  description: second\n  command: echo two\n",
        )
        .unwrap();

        let store = WorkflowYamlStore::new(dir).unwrap();
        let w = store.get("probe").unwrap();
        assert_eq!(w.steps.len(), 2);
        assert_eq!(w.base.schema, 3, "schema defaults, it is not required");
        assert_eq!(w.steps[0].command.as_deref(), Some("echo one"));
    }

    /// Negative control: what the user actually sees when a required field is
    /// missing. `#[serde(flatten)]` is known to degrade serde's missing-field
    /// messages, so assert the message still names the field.
    #[test]
    fn missing_required_field_names_the_field() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("workflows");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("probe.yaml"),
            "name: probe\ndescription: d\nsteps:\n- order: 1\n  description: s\n",
        )
        .unwrap();

        let err = format!(
            "{:#}",
            WorkflowYamlStore::new(dir)
                .unwrap()
                .get("probe")
                .unwrap_err()
        );
        assert!(
            err.contains("content"),
            "error should name the missing field, got: {err}"
        );
    }

    /// Write a skill.yaml under `<home>/skills/<name>/`.
    fn write_skill(home: &std::path::Path, name: &str, category: &str, procedure: bool) {
        let dir = home.join("skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let proc_block = if procedure {
            "\n  procedure:\n    steps:\n    - description: echo hi\n      id: s1\n      command: echo hi\n"
        } else {
            "\n"
        };
        std::fs::write(
            dir.join("skill.yaml"),
            format!(
                "name: {name}\nversion: 1.0.0\npublisher: human:test\n\
                 description: test skill\ncategory: {category}\nprovenance: human\n\
                 content:\n  abstract: t{proc_block}"
            ),
        )
        .unwrap();
    }

    #[test]
    fn find_workflow_skill_matches_exact_runnable_only() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write_skill(home, "nightly-scan", "workflow", true);
        write_skill(home, "no-procedure", "workflow", false);
        write_skill(home, "not-a-workflow", "context", true);

        // A runnable workflow skill resolves.
        assert!(find_workflow_skill(home, "nightly-scan").is_some());
        // category:Workflow without a procedure has nothing to execute.
        assert!(find_workflow_skill(home, "no-procedure").is_none());
        // Other categories are not workflows.
        assert!(find_workflow_skill(home, "not-a-workflow").is_none());
        // Exact only — a schedule fires without a TTY, and a fuzzy match
        // would be refused at run time, making the schedule a silent no-op.
        assert!(find_workflow_skill(home, "nightly").is_none());
        assert!(find_workflow_skill(home, "nightly-scan-extra").is_none());
        // Missing skills dir is not an error.
        assert!(find_workflow_skill(tmp.path().join("nope").as_path(), "x").is_none());
    }

    #[test]
    fn create_draft_workflow_persists_draft() {
        let tmp = tempfile::tempdir().unwrap();
        let store =
            crate::store::workflow_yaml::WorkflowYamlStore::new(tmp.path().join("workflows"))
                .unwrap();
        create_draft_workflow_in(
            &store,
            "test-then-commit",
            "Run tests then commit",
            "after editing code",
            &["s1".into(), "s2".into()],
        )
        .unwrap();
        assert!(store.exists("test-then-commit"));
        let wf = store.get("test-then-commit").unwrap();
        assert_eq!(wf.base.maturity, mur_common::knowledge::Maturity::Draft);
        assert_eq!(wf.trigger, "after editing code");
    }
}
