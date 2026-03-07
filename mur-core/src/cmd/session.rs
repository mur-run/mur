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
