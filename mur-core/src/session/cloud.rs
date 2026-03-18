//! Cloud sync for session recordings.
//!
//! Pushes local session recordings to the cloud server via
//! `POST /api/v1/sessions`. Uses `.synced` marker files to track
//! which sessions have already been pushed.

use anyhow::Result;
use serde::Serialize;
use std::path::PathBuf;

use super::SessionEvent;

fn recordings_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".mur")
        .join("session")
        .join("recordings")
}

fn synced_marker(id: &str) -> PathBuf {
    recordings_dir().join(format!("{}.synced", id))
}

fn is_synced(id: &str) -> bool {
    synced_marker(id).exists()
}

fn mark_synced(id: &str) -> Result<()> {
    let path = synced_marker(id);
    std::fs::write(&path, chrono::Utc::now().to_rfc3339())?;
    Ok(())
}

/// Payload sent to `POST /api/v1/sessions`.
#[derive(Serialize)]
struct SessionPushPayload {
    id: String,
    source: String,
    started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    stopped_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    tools_used: Vec<String>,
    user_turns: usize,
    assistant_turns: usize,
    events: Vec<SessionEvent>,
    file_size: u64,
}

/// Push a single session to the cloud server.
/// Returns Ok(true) if pushed, Ok(false) if skipped (already synced or missing data).
pub fn push_session(
    server_url: &str,
    token: &str,
    id: &str,
    quiet: bool,
) -> Result<bool> {
    if is_synced(id) {
        return Ok(false);
    }

    let meta = match super::load_meta_pub(id) {
        Some(m) => m,
        None => {
            if !quiet {
                eprintln!("  ⚠ Session {} has no metadata, skipping.", &id[..8.min(id.len())]);
            }
            return Ok(false);
        }
    };

    // Only push stopped sessions
    if meta.stopped_at.is_none() {
        if !quiet {
            eprintln!("  ⚠ Session {} is still active, skipping.", &id[..8.min(id.len())]);
        }
        return Ok(false);
    }

    let events = super::read_events(id)?;
    let recording_path = recordings_dir().join(format!("{}.jsonl", id));
    let file_size = std::fs::metadata(&recording_path)
        .map(|m| m.len())
        .unwrap_or(0);

    let payload = SessionPushPayload {
        id: meta.id.clone(),
        source: meta.source,
        started_at: meta.started_at,
        stopped_at: meta.stopped_at,
        title: meta.title,
        tools_used: meta.tools_used,
        user_turns: meta.user_turns,
        assistant_turns: meta.assistant_turns,
        events,
        file_size,
    };

    let body = serde_json::to_string(&payload)?;
    let url = format!("{}/api/v1/core/sessions", server_url);
    let device_id = crate::auth::get_device_id();
    let device_name = crate::auth::get_device_name();
    let device_os = crate::auth::get_device_os();

    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(&url)
        .timeout(std::time::Duration::from_secs(30))
        .header("Authorization", format!("Bearer {}", token))
        .header("X-Device-ID", device_id)
        .header("X-Device-Name", device_name)
        .header("X-Device-OS", device_os)
        .header("Content-Type", "application/json")
        .body(body)
        .send();

    match resp {
        Ok(r) if r.status().is_success() => {
            mark_synced(id)?;
            if !quiet {
                eprintln!("  ✓ Pushed session {} ({} events)", &id[..8.min(id.len())], payload.events.len());
            }
            Ok(true)
        }
        Ok(r) => {
            if !quiet {
                eprintln!("  ⚠ Push failed for session {}: HTTP {}", &id[..8.min(id.len())], r.status());
            }
            Ok(false)
        }
        Err(e) => {
            if !quiet {
                eprintln!("  ⚠ Push failed for session {}: {}", &id[..8.min(id.len())], e);
            }
            Ok(false)
        }
    }
}

/// Push all unsynced, stopped sessions to the cloud server.
/// Returns the number of sessions successfully pushed.
pub fn push_unsynced(server_url: &str, token: &str, quiet: bool) -> Result<usize> {
    let recordings = super::list_recordings()?;
    let mut pushed = 0usize;

    for rec in &recordings {
        if is_synced(&rec.id) {
            continue;
        }
        // Only push stopped sessions (has stopped_at in meta)
        if let Some(ref meta) = rec.meta
            && meta.stopped_at.is_some()
            && push_session(server_url, token, &rec.id, quiet)?
        {
            pushed += 1;
        }
    }

    if !quiet && pushed > 0 {
        eprintln!("  ☁ Pushed {} session(s) to cloud.", pushed);
    }

    Ok(pushed)
}

