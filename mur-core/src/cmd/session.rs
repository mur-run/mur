use anyhow::Result;

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
            if analyze {
                // Run fingerprint extraction on the recording
                let recording_path = dirs::home_dir()
                    .expect("no home dir")
                    .join(".mur")
                    .join("session")
                    .join("recordings")
                    .join(format!("{}.jsonl", id));

                if recording_path.exists() {
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

    if let Some(ref m) = meta {
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
