use anyhow::Result;
use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::*;
use std::io::{self, Write};

use crate::evolve;
use crate::store::workflow_yaml::WorkflowYamlStore;
use crate::store::yaml::YamlStore;

/// Run a workflow — output as executable prompt for AI consumption.
/// Accepts exact name, semantic query, or pipeline expression (w1 | w2 && w3, w4).
pub(crate) async fn cmd_workflow_run(query: &str, fail_fast: bool) -> Result<()> {
    use crate::store::embedding::{EmbeddingConfig, embed};
    use crate::store::lancedb::VectorStore;
    use mur_common::pipeline::{has_pipeline_syntax, parse_pipeline_expr};

    // Detect pipeline syntax and delegate to PipelineExecutor
    if has_pipeline_syntax(query) {
        let expr = parse_pipeline_expr(query)
            .map_err(|e| anyhow::anyhow!("pipeline parse error: {}", e))?;
        let store = WorkflowYamlStore::default_store()?;
        let executor =
            crate::executor::pipeline::PipelineExecutor::new(store).with_fail_fast(fail_fast);
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
        print_workflow_prompt(&w);
        return Ok(());
    }

    // Semantic search
    let index_path = dirs::home_dir()
        .expect("no home dir")
        .join(".mur")
        .join("index");

    let mut best_name: Option<String> = None;

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
            }
        }
    }

    // Fallback: keyword search
    if best_name.is_none() {
        let all = store.list_all()?;
        let q = query.to_lowercase();
        best_name = all
            .iter()
            .find(|w| {
                let text =
                    format!("{} {} {}", w.name, w.description, w.tools.join(" ")).to_lowercase();
                text.contains(&q)
            })
            .map(|w| w.name.clone());
    }

    match best_name {
        Some(name) => {
            let w = store.get(&name)?;
            print_workflow_prompt(&w);
        }
        None => {
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
            let default = v.default_value.as_deref().unwrap_or("-");
            println!(
                "- `{}` ({}, {}): {} — default: `{}`",
                v.name, v.var_type, req, v.description, default
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
                let default = v.default_value.as_deref().unwrap_or("-");
                println!(
                    "- `{}` ({}, {}): {} — default: `{}`",
                    v.name, v.var_type, req, v.description, default
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
                let default = v.default_value.as_deref().unwrap_or("-");
                println!(
                    "  ${} ({}): {} [{}] default={}",
                    v.name, v.var_type, v.description, req, default
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
    use crate::store::lancedb::VectorStore;

    let store = WorkflowYamlStore::default_store()?;
    let all_workflows = store.list_all()?;

    if all_workflows.is_empty() {
        println!(
            "No workflows found. Create one with `mur workflow new` or extract from a session."
        );
        return Ok(());
    }

    let index_path = dirs::home_dir()
        .expect("no home dir")
        .join(".mur")
        .join("index");

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
            eprintln!("Not logged in. Run `mur login` first.");
            return Ok(());
        }
    };

    // License check — warn but don't block (server enforces 403)
    eprintln!("  ⚠ Publishing requires a Pro+ plan. If you're on Free, the server will reject this.");

    let device_id = crate::auth::get_device_id();
    let device_name = crate::auth::get_device_name();
    let device_os = crate::auth::get_device_os();

    let yaml_content = std::fs::read_to_string(
        dirs::home_dir()
            .unwrap_or_default()
            .join(".mur")
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
            eprintln!("Not logged in. Run `mur login` first.");
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

pub(crate) fn cmd_suggest(create: bool) -> Result<()> {
    use evolve::compose::suggest_workflows_with_patterns;
    use evolve::cooccurrence::CooccurrenceMatrix;
    use evolve::decompose::{analyze_workflow_for_extraction, extract_pattern_from_step};

    let pattern_store = YamlStore::default_store()?;
    let workflow_store = WorkflowYamlStore::default_store()?;
    let patterns = pattern_store.list_all()?;
    let workflows = workflow_store.list_all()?;

    // ─── Part 1: Workflow composition from co-occurrence ─────────────

    let matrix_path = CooccurrenceMatrix::default_path();
    let matrix = CooccurrenceMatrix::load(&matrix_path)?;

    println!("🔗 Knowledge ↔ Workflow Intelligence\n");
    println!("── Co-occurrence Data ──");
    println!("  Tracked pairs: {}", matrix.pair_count());

    let suggestions = suggest_workflows_with_patterns(&matrix, 5, &patterns);

    if suggestions.is_empty() {
        println!("  No workflow composition suggestions yet.");
        println!("  (Need 3+ patterns co-occurring 5+ times)");
    } else {
        println!("\n── Workflow Composition Suggestions ──\n");
        for (i, s) in suggestions.iter().enumerate() {
            println!(
                "  {}. {} (score: {})",
                i + 1,
                s.suggested_name,
                s.cooccurrence_score,
            );
            println!("     Patterns: {}", s.patterns.join(", "));
            println!("     Trigger: {}", s.suggested_trigger);

            if create {
                if workflow_store.exists(&s.suggested_name) {
                    println!(
                        "     -> Workflow '{}' already exists, skipping.",
                        s.suggested_name
                    );
                } else {
                    // Create a draft workflow from the suggestion
                    let wf = mur_common::workflow::Workflow {
                        base: KnowledgeBase {
                            name: s.suggested_name.clone(),
                            description: format!(
                                "Auto-suggested workflow from {} co-occurring patterns",
                                s.patterns.len()
                            ),
                            content: Content::Plain(format!(
                                "Combines patterns: {}",
                                s.patterns.join(", ")
                            )),
                            tags: collect_tags_from_patterns(&s.patterns, &patterns),
                            ..Default::default()
                        },
                        steps: vec![],
                        variables: vec![],
                        source_sessions: vec![],
                        trigger: s.suggested_trigger.clone(),
                        tools: vec![],
                        published_version: 0,
                        permission: Default::default(),
        schedule: None,
                    };
                    workflow_store.save(&wf)?;
                    println!("     -> Created draft workflow: {}", s.suggested_name);

                    // Add cross-reference: link each source pattern to this workflow
                    for pname in &s.patterns {
                        if let Ok(mut p) = pattern_store.get(pname)
                            && !p.links.workflows.contains(&s.suggested_name)
                        {
                            p.base.links.workflows.push(s.suggested_name.clone());
                            let _ = pattern_store.save(&p);
                        }
                    }
                }
            }
            println!();
        }
    }

    // ─── Part 2: Workflow decomposition into patterns ────────────────

    if !workflows.is_empty() {
        println!("── Decomposition Candidates ──\n");

        let mut any_candidates = false;
        for wf in &workflows {
            let candidates = analyze_workflow_for_extraction(wf, &patterns);
            if candidates.is_empty() {
                continue;
            }
            any_candidates = true;

            println!("  Workflow: {} ({} candidates)", wf.name, candidates.len());
            for c in &candidates {
                println!("    Step {}: \"{}\"", c.step_index + 1, c.step_description,);
                println!("      -> Pattern: {}", c.suggested_pattern_name);
                println!("      Reason: {}", c.reason);

                if create {
                    if pattern_store.exists(&c.suggested_pattern_name) {
                        println!(
                            "      -> Pattern '{}' already exists, skipping.",
                            c.suggested_pattern_name
                        );
                    } else if let Some(pattern) = extract_pattern_from_step(wf, c.step_index) {
                        pattern_store.save(&pattern)?;
                        println!(
                            "      -> Created draft pattern: {}",
                            c.suggested_pattern_name
                        );
                    }
                }
            }
            println!();
        }

        if !any_candidates {
            println!("  No decomposition candidates found in existing workflows.");
        }
    }

    // ─── Summary ─────────────────────────────────────────────────────

    if !create && (!suggestions.is_empty() || !workflows.is_empty()) {
        println!("Run `mur suggest --create` to auto-create suggested items as drafts.");
    }

    Ok(())
}

/// Collect tags from a set of pattern names.
pub(crate) fn collect_tags_from_patterns(
    names: &[String],
    patterns: &[Pattern],
) -> mur_common::pattern::Tags {
    let mut topics: Vec<String> = Vec::new();
    let mut languages: Vec<String> = Vec::new();

    for name in names {
        if let Some(p) = patterns.iter().find(|p| &p.name == name) {
            for t in &p.tags.topics {
                if !topics.contains(t) {
                    topics.push(t.clone());
                }
            }
            for l in &p.tags.languages {
                if !languages.contains(l) {
                    languages.push(l.clone());
                }
            }
        }
    }

    mur_common::pattern::Tags {
        topics,
        languages,
        extra: Default::default(),
    }
}

// ─── Schedule management ────────────────────────────────────────────

pub(crate) fn cmd_schedule_list() -> Result<()> {
    let store = WorkflowYamlStore::default_store()?;
    let workflows = store.list_all()?;

    let scheduled: Vec<_> = workflows.iter().filter(|w| w.schedule.is_some()).collect();

    if scheduled.is_empty() {
        println!("📋 No scheduled workflows.");
        println!("  Use `mur workflow schedule set <name> \"0 * * * *\"` to add one.");
        return Ok(());
    }

    println!("📋 Scheduled workflows:\n");
    for wf in &scheduled {
        let cron = wf.schedule.as_deref().unwrap_or("—");
        let desc = if wf.description.is_empty() {
            &wf.name
        } else {
            &wf.description
        };
        println!("  🔄 {} — `{}`", desc, cron);
        println!("     name: {}", wf.name);
    }
    println!("\n  Total: {} scheduled workflow(s)", scheduled.len());

    Ok(())
}

pub(crate) fn cmd_schedule_set(name: &str, cron: &str) -> Result<()> {
    let store = WorkflowYamlStore::default_store()?;

    // Verify workflow exists
    let mut wf = store.get(name)?;

    // Basic cron validation (5 fields)
    let parts: Vec<&str> = cron.split_whitespace().collect();
    if parts.len() < 5 || parts.len() > 7 {
        anyhow::bail!(
            "Invalid cron expression '{}'. Expected 5-7 fields (e.g. '0 * * * *' for hourly).",
            cron
        );
    }

    wf.schedule = Some(cron.to_string());
    store.save(&wf)?;

    println!("✅ Schedule set for '{}': {}", name, cron);
    println!("   Commander daemon will pick this up within 30 seconds.");

    Ok(())
}

pub(crate) fn cmd_schedule_remove(name: &str) -> Result<()> {
    let store = WorkflowYamlStore::default_store()?;
    let mut wf = store.get(name)?;

    if wf.schedule.is_none() {
        println!("ℹ️  '{}' has no schedule.", name);
        return Ok(());
    }

    wf.schedule = None;
    store.save(&wf)?;

    println!("🗑️  Schedule removed from '{}'.", name);

    Ok(())
}

pub(crate) fn cmd_schedule_enable(name: &str, enable: bool) -> Result<()> {
    let store = WorkflowYamlStore::default_store()?;
    let mut wf = store.get(name)?;

    match &wf.schedule {
        Some(cron) if !enable => {
            // Disable: prefix with # to comment it out (convention)
            wf.schedule = Some(format!("#disabled: {}", cron));
            store.save(&wf)?;
            println!("⏸️  Schedule disabled for '{}'.", name);
        }
        Some(cron) if enable && cron.starts_with("#disabled: ") => {
            // Enable: remove the #disabled prefix
            wf.schedule = Some(cron.trim_start_matches("#disabled: ").to_string());
            store.save(&wf)?;
            println!("▶️  Schedule enabled for '{}'.", name);
        }
        Some(_) if enable => {
            println!("ℹ️  '{}' is already enabled.", name);
        }
        None => {
            println!("ℹ️  '{}' has no schedule. Use `mur workflow schedule set` first.", name);
        }
        _ => {}
    }

    Ok(())
}
