//! Session recording for Claude Code hooks.
//!
//! Records session events to append-only JSONL files for later analysis.
//! State file: `~/.mur/session/active.json`
//! Recordings: `~/.mur/session/recordings/<session-id>.jsonl`

pub mod ambient;
pub mod cloud;
pub mod scrub;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

/// Active session metadata stored in `~/.mur/session/active.json`.
#[derive(Debug, Serialize, Deserialize)]
pub struct ActiveSession {
    pub id: String,
    pub started_at: String,
    pub source: String,
}

/// A single session event appended to the JSONL recording.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionEvent {
    pub timestamp: u64,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    pub content: String,
    // ── Layer-1 enrichment (spec §3.1; all Option/default for back-compat) ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

fn session_dir() -> PathBuf {
    crate::paths::mur_root(None).join("session")
}

fn recordings_dir() -> PathBuf {
    session_dir().join("recordings")
}

fn active_path() -> PathBuf {
    session_dir().join("active.json")
}

/// Read the active session id, if any. Returns `None` when no session is active.
///
/// Used by the conversations archive ingest path to label messages with the
/// current session — without requiring callers to parse `active.json` themselves.
pub fn active_session_id() -> anyhow::Result<Option<String>> {
    let path = active_path();
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)?;
    let session: ActiveSession = serde_json::from_str(&content)?;
    Ok(Some(session.id))
}

/// Metadata about a session, persisted alongside the recording as `.meta.json`.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SessionMeta {
    pub id: String,
    pub source: String,
    pub started_at: String,
    pub stopped_at: Option<String>,
    pub title: Option<String>,
    pub tools_used: Vec<String>,
    pub user_turns: usize,
    pub assistant_turns: usize,
    // ── Ambient capture additions (all default for back-compat) ──
    /// Set by `mur in`: harvest gate passes this session unconditionally.
    #[serde(default)]
    pub marked: bool,
    /// RFC3339 of the last harvest-gate run over this session (prevents re-scan).
    #[serde(default)]
    pub gated_at: Option<String>,
    /// RFC3339 when the user accepted/skipped this session's proposal.
    #[serde(default)]
    pub harvested_at: Option<String>,
}

fn meta_path(id: &str) -> PathBuf {
    recordings_dir().join(format!("{}.meta.json", id))
}

fn load_meta(id: &str) -> Option<SessionMeta> {
    let path = meta_path(id);
    let content = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Public accessor for loading session metadata.
pub fn load_meta_pub(id: &str) -> Option<SessionMeta> {
    load_meta(id)
}

fn save_meta(meta: &SessionMeta) -> Result<()> {
    let path = meta_path(&meta.id);
    let json = serde_json::to_string_pretty(meta)?;
    fs::write(&path, json).context("Failed to write session meta")?;
    Ok(())
}

/// Returns true if the event should be skipped (noise).
pub fn should_skip(event_type: &str, content: &str) -> bool {
    let trimmed = content.trim();

    // Skip empty assistant responses or marker-only responses
    if event_type == "assistant" {
        if trimmed.is_empty() {
            return true;
        }
        if trimmed == "[stop: turn_end]"
            || trimmed == "[stop: end_turn]"
            || trimmed.starts_with("[stop:")
        {
            return true;
        }
    }

    // Skip mur's own session management commands in tool_call events
    if event_type == "tool_call"
        && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed)
        && let Some(cmd) = parsed.get("command").and_then(|v| v.as_str())
        && cmd.starts_with("mur session")
    {
        return true;
    }

    false
}

/// Start a new recording session.
pub fn start(source: &str) -> Result<ActiveSession> {
    let dir = session_dir();
    fs::create_dir_all(&dir)?;
    fs::create_dir_all(recordings_dir())?;

    // Fail if already recording
    let active = active_path();
    if active.exists() {
        anyhow::bail!("Session already active. Run `mur session stop` first.");
    }

    let session = ActiveSession {
        id: uuid::Uuid::new_v4().to_string(),
        started_at: chrono::Utc::now().to_rfc3339(),
        source: source.to_string(),
    };

    let json = serde_json::to_string_pretty(&session)?;
    fs::write(&active, json).context("Failed to write active session file")?;

    // Create empty recording file
    let recording = recordings_dir().join(format!("{}.jsonl", session.id));
    fs::File::create(&recording).context("Failed to create recording file")?;

    // Create initial session meta
    let meta = SessionMeta {
        id: session.id.clone(),
        source: session.source.clone(),
        started_at: session.started_at.clone(),
        stopped_at: None,
        title: None,
        tools_used: vec![],
        user_turns: 0,
        assistant_turns: 0,
        marked: false,
        gated_at: None,
        harvested_at: None,
    };
    save_meta(&meta)?;

    Ok(session)
}

/// Stop the active session. Returns the session ID if one was active.
pub fn stop() -> Result<Option<String>> {
    let active = active_path();
    if !active.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&active)?;
    let session: ActiveSession = serde_json::from_str(&content)?;
    let id = session.id.clone();

    // Update meta with stopped_at
    if let Some(mut meta) = load_meta(&id) {
        meta.stopped_at = Some(chrono::Utc::now().to_rfc3339());
        let _ = save_meta(&meta);
    }

    fs::remove_file(&active)?;
    Ok(Some(id))
}

/// Append one event to `<recordings_dir>/<id>.jsonl`, creating the meta file on
/// first write. Shared by the legacy active-session path and ambient capture.
pub(crate) fn record_event_in_dir(
    recordings_dir: &std::path::Path,
    id: &str,
    source: &str,
    event: &SessionEvent,
) -> Result<()> {
    fs::create_dir_all(recordings_dir)?;

    let recording_path = recordings_dir.join(format!("{}.jsonl", id));
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&recording_path)
        .context("Failed to open recording file")?;

    let mut line = serde_json::to_string(event)?;
    line.push('\n');
    file.write_all(line.as_bytes())?;

    // Load-or-create meta, then apply the same turn/tool accounting as before.
    let meta_path = recordings_dir.join(format!("{}.meta.json", id));
    let mut meta: SessionMeta = fs::read_to_string(&meta_path)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_else(|| SessionMeta {
            id: id.to_string(),
            source: source.to_string(),
            started_at: chrono::Utc::now().to_rfc3339(),
            stopped_at: None,
            title: None,
            tools_used: vec![],
            user_turns: 0,
            assistant_turns: 0,
            marked: false,
            gated_at: None,
            harvested_at: None,
        });

    match event.event_type.as_str() {
        "user" => {
            meta.user_turns += 1;
            if meta.title.is_none() {
                let title: String = event.content.chars().take(80).collect();
                meta.title = Some(title);
            }
        }
        "assistant" => meta.assistant_turns += 1,
        "tool_call" => {
            if let Some(tool_name) = &event.tool {
                let tools: BTreeSet<String> = meta.tools_used.iter().cloned().collect();
                if !tools.contains(tool_name) {
                    meta.tools_used.push(tool_name.clone());
                }
            }
        }
        _ => {}
    }
    let json = serde_json::to_string_pretty(&meta)?;
    fs::write(&meta_path, json).context("Failed to write session meta")?;
    Ok(())
}

/// Record an event to the active session. Returns Ok(false) if no session is active.
pub fn record(event_type: &str, tool: Option<&str>, content: &str) -> Result<bool> {
    let active = active_path();
    if !active.exists() {
        return Ok(false);
    }

    // Skip noise events
    if should_skip(event_type, content) {
        return Ok(true);
    }

    let session_content = fs::read_to_string(&active)?;
    let session: ActiveSession = serde_json::from_str(&session_content)?;

    let event = SessionEvent {
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        event_type: event_type.to_string(),
        tool: tool.map(|s| s.to_string()),
        content: content.to_string(),
        working_dir: None,
        git_branch: None,
        exit_code: None,
    };
    record_event_in_dir(&recordings_dir(), &session.id, &session.source, &event)?;
    Ok(true)
}

/// Get the active session, if any.
pub fn get_active() -> Result<Option<ActiveSession>> {
    let active = active_path();
    if !active.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&active)?;
    let session: ActiveSession = serde_json::from_str(&content)?;
    Ok(Some(session))
}

/// Information about a past recording.
#[derive(Debug)]
pub struct RecordingInfo {
    pub id: String,
    pub event_count: usize,
    pub file_size: u64,
    pub modified: std::time::SystemTime,
    pub meta: Option<SessionMeta>,
}

/// Update the title or other fields in a session's meta.
/// Creates meta if it doesn't exist yet.
pub fn update_meta(id: &str, title: Option<String>) -> Result<SessionMeta> {
    let mut meta =
        load_meta(id).ok_or_else(|| anyhow::anyhow!("No meta found for session '{}'", id))?;
    if let Some(t) = title {
        meta.title = Some(t);
    }
    save_meta(&meta)?;
    Ok(meta)
}

/// Delete a session's recording and meta files.
pub fn delete_recording(id: &str) -> Result<()> {
    let jsonl = recordings_dir().join(format!("{}.jsonl", id));
    let meta = meta_path(id);
    if jsonl.exists() {
        fs::remove_file(&jsonl)?;
    }
    if meta.exists() {
        fs::remove_file(&meta)?;
    }
    Ok(())
}

/// Remove a session's recording, metadata, and sync marker from a specific
/// recordings directory.  Used by `remove_recording()` and by tests.
fn remove_recording_in_dir(recordings_dir: &std::path::Path, id: &str) -> Result<()> {
    let jsonl = recordings_dir.join(format!("{}.jsonl", id));
    let meta = recordings_dir.join(format!("{}.meta.json", id));
    let synced = recordings_dir.join(format!("{}.synced", id));

    if jsonl.exists() {
        fs::remove_file(&jsonl)?;
    }
    if meta.exists() {
        fs::remove_file(&meta)?;
    }
    if synced.exists() {
        fs::remove_file(&synced)?;
    }
    Ok(())
}

/// Remove a session recording, metadata, and sync marker from the default
/// recordings directory.
pub(crate) fn remove_recording(id: &str) -> Result<()> {
    remove_recording_in_dir(&recordings_dir(), id)
}

/// Remove recordings older than `retention_days`, judged by meta `started_at`.
/// Marked-but-never-harvested sessions are kept regardless of age (the user
/// declared them important; they leave via harvest or explicit remove).
/// Returns the number of recordings removed.
pub fn gc_in_dir(recordings_dir: &std::path::Path, retention_days: u32) -> Result<usize> {
    if !recordings_dir.exists() {
        return Ok(0);
    }
    let cutoff = chrono::Utc::now() - chrono::Duration::days(retention_days as i64);
    let mut removed = 0usize;
    for entry in fs::read_dir(recordings_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else {
            continue;
        };
        let meta = fs::read_to_string(recordings_dir.join(format!("{}.meta.json", id)))
            .ok()
            .and_then(|c| serde_json::from_str::<SessionMeta>(&c).ok());
        let Some(meta) = meta else { continue }; // metaless files: leave for manual cleanup
        let Ok(started) = chrono::DateTime::parse_from_rfc3339(&meta.started_at) else {
            continue;
        };
        if started.with_timezone(&chrono::Utc) >= cutoff {
            continue;
        }
        if meta.marked && meta.harvested_at.is_none() {
            continue;
        }
        remove_recording_in_dir(recordings_dir, &id)?;
        removed += 1;
    }
    Ok(removed)
}

/// Set or clear the `marked` flag on a session's meta.
pub fn update_marked(id: &str, marked: bool) -> Result<SessionMeta> {
    let mut meta =
        load_meta(id).ok_or_else(|| anyhow::anyhow!("No meta found for session '{}'", id))?;
    meta.marked = marked;
    save_meta(&meta)?;
    Ok(meta)
}

/// Check if a recording has been synced to the cloud.
pub(crate) fn is_recording_synced(id: &str) -> bool {
    recordings_dir().join(format!("{}.synced", id)).exists()
}

/// Read and parse all events from a session recording.
pub fn read_events(id: &str) -> Result<Vec<SessionEvent>> {
    let path = recordings_dir().join(format!("{}.jsonl", id));
    if !path.exists() {
        anyhow::bail!("Recording not found: {}", id);
    }

    let content = fs::read_to_string(&path).context("Failed to read recording file")?;
    let mut events = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let event: SessionEvent =
            serde_json::from_str(line).context("Failed to parse session event")?;
        events.push(event);
    }
    Ok(events)
}

/// Find a full session ID from a prefix.
pub fn find_recording_by_prefix(prefix: &str) -> Result<Option<String>> {
    let dir = recordings_dir();
    if !dir.exists() {
        return Ok(None);
    }
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            && stem.starts_with(prefix)
        {
            return Ok(Some(stem.to_string()));
        }
    }
    Ok(None)
}

/// List past session recordings.
pub fn list_recordings() -> Result<Vec<RecordingInfo>> {
    let dir = recordings_dir();
    if !dir.exists() {
        return Ok(vec![]);
    }

    let mut recordings = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }

        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        let metadata = entry.metadata()?;
        let file_size = metadata.len();
        let modified = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);

        // Count events by counting non-empty lines
        let content = fs::read_to_string(&path).unwrap_or_default();
        let event_count = content.lines().filter(|l| !l.trim().is_empty()).count();

        let meta = load_meta(&id);

        recordings.push(RecordingInfo {
            id,
            event_count,
            file_size,
            modified,
            meta,
        });
    }

    // Sort by modified time, newest first
    recordings.sort_by_key(|r| std::cmp::Reverse(r.modified));

    Ok(recordings)
}

#[cfg(test)]
mod tests;
