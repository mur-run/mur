use anyhow::Result;
use std::collections::BTreeSet;

use crate::session;

pub(crate) fn cmd_session_start(source: &str) -> Result<()> {
    let session = session::start(source)?;
    eprintln!("Session started: {} (source: {})", &session.id[..8], source);
    Ok(())
}

pub(crate) fn cmd_session_stop(analyze: bool) -> Result<()> {
    match session::stop()? {
        Some(id) => {
            eprintln!("Session stopped: {}", &id[..8]);

            let recording_path = dirs::home_dir()
                .expect("no home dir")
                .join(".mur")
                .join("session")
                .join("recordings")
                .join(format!("{}.jsonl", id));

            if analyze && recording_path.exists() {
                let content = std::fs::read_to_string(&recording_path)?;
                if !content.trim().is_empty() {
                    use crate::capture::emergence::{extract_fingerprints, save_fingerprints};
                    let fps = extract_fingerprints(&content, &id);
                    if !fps.is_empty() {
                        save_fingerprints(&fps)?;
                        eprintln!("Extracted {} fingerprints from session.", fps.len());
                    }
                }
            }

            // Auto-push to device sync if configured
            if let Ok(config) = crate::store::config::load_config()
                && config.sync.auto
                && config.sync.method != "local"
                && let Err(e) =
                    super::sync_cmd::device_sync(true, super::sync_cmd::DeviceSyncDirection::Push)
            {
                eprintln!("  ⚠ Auto-push failed: {}", e);
            }

            // Interactive post-session menu (only in terminal)
            if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
                // Show session summary
                let meta = session::load_meta_pub(&id);
                let event_count = if recording_path.exists() {
                    std::fs::read_to_string(&recording_path)
                        .map(|c| c.lines().filter(|l| !l.trim().is_empty()).count())
                        .unwrap_or(0)
                } else {
                    0
                };

                eprintln!();
                eprintln!("Session summary:");
                eprintln!("  Events: {}", event_count);
                if let Some(ref m) = meta {
                    eprintln!(
                        "  Turns:  {} user, {} assistant",
                        m.user_turns, m.assistant_turns
                    );
                    if let (Some(stopped), Ok(start)) = (
                        &m.stopped_at,
                        chrono::DateTime::parse_from_rfc3339(&m.started_at),
                    ) && let Ok(end) = chrono::DateTime::parse_from_rfc3339(stopped)
                    {
                        let secs = end.signed_duration_since(start).num_seconds();
                        if secs >= 3600 {
                            eprintln!(
                                "  Duration: {}h {}m {}s",
                                secs / 3600,
                                (secs % 3600) / 60,
                                secs % 60
                            );
                        } else if secs >= 60 {
                            eprintln!("  Duration: {}m {}s", secs / 60, secs % 60);
                        } else {
                            eprintln!("  Duration: {}s", secs);
                        }
                    }
                }

                let items = &[
                    "🔍 Analyze — extract patterns with LLM (needs reasoning model)",
                    "📦 Export — save as markdown",
                    "⏭  Skip",
                ];

                if let Ok(choice) = dialoguer::Select::new()
                    .with_prompt("What next?")
                    .items(items)
                    .default(2)
                    .interact()
                {
                    let exe =
                        std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("mur"));
                    match choice {
                        0 => {
                            // Analyze: run `mur learn extract --file <path> --llm`
                            if let Some(path_str) = recording_path.to_str() {
                                let status = std::process::Command::new(&exe)
                                    .args(["learn", "extract", "--file", path_str, "--llm"])
                                    .status();
                                if let Ok(s) = status {
                                    if !s.success() {
                                        eprintln!("  ⚠ learn extract exited with {}", s);
                                    }
                                } else if let Err(e) = status {
                                    eprintln!("  ⚠ Failed to run learn extract: {}", e);
                                }
                            }
                        }
                        1 => {
                            // Export: run `mur session export <id> --format markdown`
                            let status = std::process::Command::new(&exe)
                                .args(["session", "export", &id, "--format", "markdown"])
                                .status();
                            if let Ok(s) = status {
                                if !s.success() {
                                    eprintln!("  ⚠ session export exited with {}", s);
                                }
                            } else if let Err(e) = status {
                                eprintln!("  ⚠ Failed to run session export: {}", e);
                            }
                        }
                        _ => {
                            // Skip — do nothing
                        }
                    }
                }
            }
        }
        None => {
            eprintln!("No active session.");
        }
    }
    Ok(())
}

pub(crate) fn cmd_session_record(
    event_type: &str,
    tool: Option<&str>,
    content: &str,
) -> Result<()> {
    // Validate event type
    match event_type {
        "user" | "assistant" | "tool_call" | "tool_result" => {}
        _ => anyhow::bail!(
            "Invalid event type '{}'. Use: user, assistant, tool_call, tool_result",
            event_type
        ),
    }

    if !session::record(event_type, tool, content)? {
        // No active session — silently succeed (hooks shouldn't fail)
        return Ok(());
    }
    Ok(())
}

pub(crate) fn cmd_session_status() -> Result<()> {
    match session::get_active()? {
        Some(session) => {
            println!("Active session: {}", session.id);
            println!("  Started: {}", session.started_at);
            println!("  Source:  {}", session.source);

            // Count events in the recording
            let recording_path = dirs::home_dir()
                .expect("no home dir")
                .join(".mur")
                .join("session")
                .join("recordings")
                .join(format!("{}.jsonl", session.id));

            if recording_path.exists() {
                let content = std::fs::read_to_string(&recording_path).unwrap_or_default();
                let count = content.lines().filter(|l| !l.trim().is_empty()).count();
                println!("  Events:  {}", count);
            }
        }
        None => {
            println!("No active session.");
        }
    }
    Ok(())
}

pub(crate) fn cmd_session_review(id_prefix: &str) -> Result<()> {
    let full_id = session::find_recording_by_prefix(id_prefix)?
        .ok_or_else(|| anyhow::anyhow!("No session found matching prefix '{}'", id_prefix))?;

    let port = 3847u16;
    let url = format!("http://localhost:{}/#/sessions/{}/review", port, full_id);

    // Check if server is already running
    let server_running = std::net::TcpStream::connect(format!("127.0.0.1:{}", port)).is_ok();

    if !server_running {
        eprintln!("Starting server on port {}...", port);
        // Start server in the background
        let exe = std::env::current_exe().unwrap_or_else(|_| "mur".into());
        std::process::Command::new(exe)
            .args(["serve", "--port", &port.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok();
        // Brief wait for server to bind
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    eprintln!(
        "Opening session {} in browser...",
        &full_id[..8.min(full_id.len())]
    );

    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&url).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", &url])
            .spawn();
    }

    Ok(())
}

pub(crate) fn cmd_session_show(id_prefix: &str, last: Option<usize>, json: bool) -> Result<()> {
    let full_id = session::find_recording_by_prefix(id_prefix)?
        .ok_or_else(|| anyhow::anyhow!("No session found matching prefix '{}'", id_prefix))?;

    let meta = session::load_meta_pub(&full_id);
    let events = session::read_events(&full_id)?;

    if json {
        #[derive(serde::Serialize)]
        struct SessionJson {
            id: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            meta: Option<session::SessionMeta>,
            events: Vec<session::SessionEvent>,
        }

        let display_events = if let Some(n) = last {
            events
                .into_iter()
                .rev()
                .take(n)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect()
        } else {
            events
        };

        let output = SessionJson {
            id: full_id,
            meta,
            events: display_events,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    // Pretty-print header
    let short_id = if full_id.len() > 8 {
        &full_id[..8]
    } else {
        &full_id
    };
    println!("Session {}", short_id);
    println!("{}", "─".repeat(60));

    if let Some(m) = meta {
        println!("  ID:      {}", m.id);
        println!("  Source:  {}", m.source);
        if let Some(ref title) = m.title {
            println!("  Title:   {}", title);
        }
        println!("  Started: {}", m.started_at);
        if let Some(ref stopped) = m.stopped_at {
            println!("  Stopped: {}", stopped);
            // Calculate duration
            if let (Ok(start), Ok(end)) = (
                chrono::DateTime::parse_from_rfc3339(&m.started_at),
                chrono::DateTime::parse_from_rfc3339(stopped),
            ) {
                let dur = end.signed_duration_since(start);
                let secs = dur.num_seconds();
                if secs >= 3600 {
                    println!(
                        "  Duration: {}h {}m {}s",
                        secs / 3600,
                        (secs % 3600) / 60,
                        secs % 60
                    );
                } else if secs >= 60 {
                    println!("  Duration: {}m {}s", secs / 60, secs % 60);
                } else {
                    println!("  Duration: {}s", secs);
                }
            }
        } else {
            println!("  Stopped: (still active)");
        }
        println!(
            "  Turns:   {} user, {} assistant",
            m.user_turns, m.assistant_turns
        );
        if !m.tools_used.is_empty() {
            println!("  Tools:   {}", m.tools_used.join(", "));
        }
    } else {
        println!("  ID: {}", full_id);
        println!("  (no metadata file found)");
    }

    println!();

    // Determine events to display
    let display_events: Vec<&session::SessionEvent> = if let Some(n) = last {
        events
            .iter()
            .rev()
            .take(n)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    } else {
        events.iter().collect()
    };

    if display_events.is_empty() {
        println!("  (no events)");
        return Ok(());
    }

    // Use first event timestamp as baseline for relative time
    let base_ts = events.first().map(|e| e.timestamp).unwrap_or(0);

    // Detect if stdout is a terminal (for content truncation)
    let is_tty = std::io::IsTerminal::is_terminal(&std::io::stdout());

    println!("Events ({}):", display_events.len());
    println!();

    for event in &display_events {
        let rel_ms = event.timestamp.saturating_sub(base_ts);
        let rel_secs = rel_ms / 1000;
        let time_str = if rel_secs >= 3600 {
            format!(
                "+{}:{:02}:{:02}",
                rel_secs / 3600,
                (rel_secs % 3600) / 60,
                rel_secs % 60
            )
        } else {
            format!("+{:02}:{:02}", rel_secs / 60, rel_secs % 60)
        };

        let (icon, label) = match event.event_type.as_str() {
            "user" => ("👤", "user".to_string()),
            "assistant" => ("🤖", "assistant".to_string()),
            "tool_call" => (
                "🔧",
                if let Some(ref t) = event.tool {
                    format!("tool_call({})", t)
                } else {
                    "tool_call".to_string()
                },
            ),
            "tool_result" => ("📋", "tool_result".to_string()),
            _ => ("  ", event.event_type.clone()),
        };

        let content = if is_tty && event.content.len() > 200 {
            format!("{}…", &event.content[..200])
        } else {
            event.content.clone()
        };

        // Replace newlines with spaces for single-line display
        let content = content.replace('\n', " ").replace('\r', "");

        println!("  {} {} {} {}", time_str, icon, label, content);
    }

    Ok(())
}

pub(crate) fn cmd_session_list() -> Result<()> {
    let recordings = session::list_recordings()?;

    if recordings.is_empty() {
        println!("No session recordings found.");
        return Ok(());
    }

    println!("Session recordings ({}):\n", recordings.len());
    for r in &recordings {
        let time: chrono::DateTime<chrono::Utc> = r.modified.into();
        let short_id = if r.id.len() > 8 { &r.id[..8] } else { &r.id };
        println!(
            "  {} — {} events, {} bytes ({})",
            short_id,
            r.event_count,
            r.file_size,
            time.format("%Y-%m-%d %H:%M"),
        );
    }
    Ok(())
}

pub(crate) fn cmd_session_export(
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
        "skill" => export_skill(&full_id, &meta, &events)?,
        _ => anyhow::bail!("Unknown format '{}'. Use: json, markdown, skill", format),
    };

    if analyze {
        let recording_path = dirs::home_dir()
            .expect("no home dir")
            .join(".mur")
            .join("session")
            .join("recordings")
            .join(format!("{}.jsonl", full_id));

        if recording_path.exists() {
            let content = std::fs::read_to_string(&recording_path)?;
            if !content.trim().is_empty() {
                use crate::capture::emergence::extract_fingerprints;
                let fps = extract_fingerprints(&content, &full_id);
                eprintln!("Extracted {} fingerprints from session.", fps.len());
            }
        }
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
