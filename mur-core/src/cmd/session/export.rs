use super::*;

pub(crate) async fn cmd_session_export(
    id_prefix: &str,
    format: &str,
    analyze: bool,
    output: Option<String>,
) -> Result<()> {
    let full_id = session::find_recording_by_prefix(id_prefix)?
        .ok_or_else(|| anyhow::anyhow!("No session found matching prefix '{}'", id_prefix))?;

    let meta = session::load_meta_pub(&full_id);
    let events = session::read_events(&full_id)?;

    let result = match format {
        "json" => export_json(&full_id, &meta, &events)?,
        "markdown" => export_markdown(&full_id, &meta, &events)?,
        "skill" => {
            if crate::extract::has_llm_config() {
                eprintln!("Using LLM-enhanced extraction (Haiku)...");
                export_skill_llm(&full_id, &events).await?
            } else {
                export_skill(&full_id, &meta, &events)?
            }
        }
        _ => anyhow::bail!("Unknown format '{}'. Use: json, markdown, skill", format),
    };

    if analyze && let Err(e) = analyze_session_to_draft(&full_id).await {
        eprintln!("  ⚠ Analyze failed: {}", e);
    }

    if let Some(path) = output {
        std::fs::write(&path, &result)?;
        eprintln!("Exported to {}", path);
    } else {
        print!("{}", result);
    }

    Ok(())
}

fn export_json(
    id: &str,
    meta: &Option<session::SessionMeta>,
    events: &[session::SessionEvent],
) -> Result<String> {
    #[derive(serde::Serialize)]
    struct SessionExport {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        meta: Option<session::SessionMeta>,
        events: Vec<session::SessionEvent>,
    }

    let output = SessionExport {
        id: id.to_string(),
        meta: meta.clone(),
        events: events.to_vec(),
    };
    Ok(serde_json::to_string_pretty(&output)?)
}

fn export_markdown(
    id: &str,
    meta: &Option<session::SessionMeta>,
    events: &[session::SessionEvent],
) -> Result<String> {
    let mut out = String::new();

    // Header
    let title = meta.as_ref().and_then(|m| m.title.as_deref()).unwrap_or(id);
    out.push_str(&format!("# Session: {}\n\n", title));

    if let Some(m) = meta {
        out.push_str(&format!("- **Source:** {}\n", m.source));
        out.push_str(&format!(
            "- **Started:** {}\n",
            format_timestamp_human(&m.started_at)
        ));

        if let Some(stopped) = &m.stopped_at
            && let (Ok(start), Ok(end)) = (
                chrono::DateTime::parse_from_rfc3339(&m.started_at),
                chrono::DateTime::parse_from_rfc3339(stopped),
            )
        {
            let dur = end.signed_duration_since(start);
            let secs = dur.num_seconds();
            let duration_str = if secs >= 3600 {
                format!("{}h {}m {}s", secs / 3600, (secs % 3600) / 60, secs % 60)
            } else if secs >= 60 {
                format!("{}m {}s", secs / 60, secs % 60)
            } else {
                format!("{}s", secs)
            };
            out.push_str(&format!("- **Duration:** {}\n", duration_str));
        }

        if !m.tools_used.is_empty() {
            out.push_str(&format!("- **Tools:** {}\n", m.tools_used.join(", ")));
        }
    }

    out.push_str("\n## Timeline\n\n");

    for event in events {
        let ts = format_epoch_ms(event.timestamp);
        let (icon, label) = match event.event_type.as_str() {
            "user" => ("\u{1f464}", "User"),
            "assistant" => ("\u{1f916}", "Assistant"),
            "tool_call" => {
                if let Some(ref t) = event.tool {
                    ("\u{1f527}", t.as_str())
                } else {
                    ("\u{1f527}", "Tool")
                }
            }
            "tool_result" => ("\u{1f4cb}", "Tool Result"),
            _ => ("", event.event_type.as_str()),
        };

        let heading = if event.event_type == "tool_call" {
            if let Some(ref t) = event.tool {
                format!("### {} Tool: {} ({})\n\n", icon, t, ts)
            } else {
                format!("### {} Tool ({})\n\n", icon, ts)
            }
        } else {
            format!("### {} {} ({})\n\n", icon, label, ts)
        };

        out.push_str(&heading);
        out.push_str(&event.content);
        out.push_str("\n\n");
    }

    Ok(out)
}

fn export_skill(
    id: &str,
    meta: &Option<session::SessionMeta>,
    events: &[session::SessionEvent],
) -> Result<String> {
    let title = meta
        .as_ref()
        .and_then(|m| m.title.as_deref())
        .unwrap_or("Untitled workflow");

    // Derive a slug name from the title
    let name: String = title
        .chars()
        .take(40)
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    let name = if name.is_empty() {
        "session-workflow".to_string()
    } else {
        name
    };

    // Collect tool_call events as steps
    let mut steps = Vec::new();
    let mut tools_used = BTreeSet::new();
    let mut order = 1u32;

    for event in events {
        if event.event_type == "tool_call" {
            let tool_name = event.tool.clone().unwrap_or_else(|| "unknown".to_string());
            tools_used.insert(tool_name.clone());

            // Truncate long content for the description
            let desc: String = event.content.chars().take(120).collect();

            steps.push(format!(
                "  - order: {}\n    description: \"{}\"\n    tool: \"{}\"",
                order,
                desc.replace('\"', "\\\"").replace('\n', " "),
                tool_name,
            ));
            order += 1;
        }
    }

    let mut out = String::new();
    out.push_str(&format!("name: \"{}\"\n", name));
    out.push_str(&format!(
        "description: \"Workflow extracted from session {}\"\n",
        &id[..8.min(id.len())]
    ));
    out.push_str("tier: session\n");
    out.push_str("importance: 0.5\n");
    out.push_str("confidence: 0.3\n");
    out.push_str(&format!(
        "tags: [\"extracted\", \"session\"{}]\n",
        if tools_used.is_empty() {
            String::new()
        } else {
            format!(
                ", {}",
                tools_used
                    .iter()
                    .map(|t| format!("\"{}\"", t.to_lowercase()))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    ));

    if steps.is_empty() {
        out.push_str("steps: []\n");
    } else {
        out.push_str("steps:\n");
        for step in &steps {
            out.push_str(step);
            out.push('\n');
        }
    }

    out.push_str(&format!(
        "tools: [{}]\n",
        tools_used
            .iter()
            .map(|t| format!("\"{}\"", t))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    out.push_str(&format!("source_sessions: [\"{}\"]\n", id));
    out.push_str("trigger: \"\"\n");

    Ok(out)
}

/// LLM-enhanced skill export using extract_workflow_llm.
async fn export_skill_llm(id: &str, events: &[session::SessionEvent]) -> Result<String> {
    let extracted = crate::extract::extract_workflow_llm(id, events).await?;
    let w = &extracted.workflow;

    let mut out = String::new();
    out.push_str(&format!("name: \"{}\"\n", w.base.name));
    out.push_str(&format!(
        "description: \"{}\"\n",
        w.base.description.replace('\"', "\\\"")
    ));
    out.push_str("tier: session\n");
    out.push_str("importance: 0.5\n");
    out.push_str("confidence: 0.5\n");

    // Tags
    let mut tags = vec![
        "extracted".to_string(),
        "session".to_string(),
        "llm-enhanced".to_string(),
    ];
    for t in &w.tools {
        tags.push(t.to_lowercase());
    }
    out.push_str(&format!(
        "tags: [{}]\n",
        tags.iter()
            .map(|t| format!("\"{}\"", t))
            .collect::<Vec<_>>()
            .join(", ")
    ));

    // Steps
    if w.steps.is_empty() {
        out.push_str("steps: []\n");
    } else {
        out.push_str("steps:\n");
        for step in &w.steps {
            out.push_str(&format!(
                "  - order: {}\n    description: \"{}\"\n",
                step.order,
                step.description.replace('\"', "\\\"").replace('\n', " "),
            ));
            if let Some(ref tool) = step.tool {
                out.push_str(&format!("    tool: \"{}\"\n", tool));
            }
        }
    }

    out.push_str(&format!(
        "tools: [{}]\n",
        w.tools
            .iter()
            .map(|t| format!("\"{}\"", t))
            .collect::<Vec<_>>()
            .join(", ")
    ));

    // Variables
    if !w.variables.is_empty() {
        out.push_str("variables:\n");
        for var in &w.variables {
            out.push_str(&format!(
                "  - name: \"{}\"\n    description: \"{}\"\n",
                var.name,
                var.description
                    .as_deref()
                    .unwrap_or("")
                    .replace('\"', "\\\""),
            ));
            if let Some(ref dv) = var.default {
                out.push_str(&format!("    default: \"{}\"\n", dv));
            }
        }
    }

    out.push_str(&format!("source_sessions: [\"{}\"]\n", id));
    out.push_str(&format!(
        "trigger: \"{}\"\n",
        w.trigger.replace('\"', "\\\"")
    ));

    Ok(out)
}

pub(crate) async fn cmd_session_push(id_prefix: Option<&str>, all: bool) -> Result<()> {
    let config = crate::store::config::load_config()?;
    let server_url = &config.server.url;
    let token = match crate::auth::load_tokens() {
        Some(t) => t.access_token,
        None => {
            eprintln!("Not authenticated. Run `mur auth login` first.");
            return Ok(());
        }
    };

    if all {
        let pushed = session::cloud::push_unsynced(server_url, &token, false).await?;
        if pushed == 0 {
            eprintln!("All sessions already synced.");
        } else {
            eprintln!();
            eprintln!("📊 Review: https://dashboard.mur.run/#/sessions");
        }
    } else if let Some(prefix) = id_prefix {
        let full_id = session::find_recording_by_prefix(prefix)?
            .ok_or_else(|| anyhow::anyhow!("No session found matching prefix '{}'", prefix))?;

        if session::cloud::push_session(server_url, &token, &full_id, false).await? {
            eprintln!();
            eprintln!(
                "📊 Review: https://dashboard.mur.run/#/sessions/{}/review",
                full_id
            );
        }
    } else {
        // No ID and no --all: push the most recent stopped session
        let recordings = session::list_recordings()?;
        let recent = recordings
            .iter()
            .find(|r| r.meta.as_ref().is_some_and(|m| m.stopped_at.is_some()));

        match recent {
            Some(r) => {
                if session::cloud::push_session(server_url, &token, &r.id, false).await? {
                    eprintln!();
                    eprintln!(
                        "📊 Review: https://dashboard.mur.run/#/sessions/{}/review",
                        r.id
                    );
                } else {
                    eprintln!("Session already synced or skipped.");
                }
            }
            None => {
                eprintln!("No stopped sessions to push.");
            }
        }
    }

    Ok(())
}

/// Format an RFC 3339 timestamp into a short human-readable form.
fn format_timestamp_human(rfc3339: &str) -> String {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(rfc3339) {
        dt.format("%Y-%m-%d %H:%M").to_string()
    } else {
        rfc3339.to_string()
    }
}

/// Format epoch milliseconds into HH:MM:SS.
fn format_epoch_ms(epoch_ms: u64) -> String {
    let secs = epoch_ms / 1000;
    let hours = (secs / 3600) % 24;
    let mins = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}", hours, mins, s)
}
