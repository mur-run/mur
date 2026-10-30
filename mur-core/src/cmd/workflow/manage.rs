//! `mur workflow list / show / search / new / publish / install`.

use super::*;

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
                    .await
                    .map_err(|e| {
                        crate::store::vector::unreadable::hinted(
                            e,
                            crate::store::vector::unreadable::hint::PATTERNS,
                        )
                    })?;

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
