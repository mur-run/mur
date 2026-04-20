//! Session recording for Claude Code hooks.
//!
//! Records session events to append-only JSONL files for later analysis.
//! State file: `~/.mur/session/active.json`
//! Recordings: `~/.mur/session/recordings/<session-id>.jsonl`

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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEvent {
    pub timestamp: u64,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    pub content: String,
}

fn session_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".mur")
        .join("session")
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
    };

    let recording_path = recordings_dir().join(format!("{}.jsonl", session.id));
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&recording_path)
        .context("Failed to open recording file")?;

    let mut line = serde_json::to_string(&event)?;
    line.push('\n');
    file.write_all(line.as_bytes())?;

    // Update session meta
    if let Some(mut meta) = load_meta(&session.id) {
        match event_type {
            "user" => {
                meta.user_turns += 1;
                if meta.title.is_none() {
                    let title: String = content.chars().take(80).collect();
                    meta.title = Some(title);
                }
            }
            "assistant" => {
                meta.assistant_turns += 1;
            }
            "tool_call" => {
                if let Some(tool_name) = tool {
                    let tools: BTreeSet<String> = meta.tools_used.iter().cloned().collect();
                    if !tools.contains(tool_name) {
                        meta.tools_used.push(tool_name.to_string());
                    }
                }
            }
            _ => {}
        }
        let _ = save_meta(&meta);
    }

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
mod tests {
    use super::*;

    #[test]
    fn test_active_session_roundtrip() {
        let session = ActiveSession {
            id: "test-123".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            source: "claude-code".to_string(),
        };

        let json = serde_json::to_string_pretty(&session).unwrap();
        let parsed: ActiveSession = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "test-123");
        assert_eq!(parsed.source, "claude-code");
        assert_eq!(parsed.started_at, "2026-01-01T00:00:00Z");
    }

    #[test]
    fn test_session_file_operations() {
        let tmp = tempfile::TempDir::new().unwrap();
        let session_dir = tmp.path().join("session");
        let recordings_dir = session_dir.join("recordings");
        fs::create_dir_all(&recordings_dir).unwrap();

        // Write active session
        let session = ActiveSession {
            id: "abc-123".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            source: "test".to_string(),
        };
        let active_path = session_dir.join("active.json");
        fs::write(
            &active_path,
            serde_json::to_string_pretty(&session).unwrap(),
        )
        .unwrap();

        // Verify it can be read back
        let content = fs::read_to_string(&active_path).unwrap();
        let parsed: ActiveSession = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.id, "abc-123");

        // Write events to JSONL
        let recording_path = recordings_dir.join("abc-123.jsonl");
        let events = vec![
            SessionEvent {
                timestamp: 1000,
                event_type: "user".to_string(),
                tool: None,
                content: "hello".to_string(),
            },
            SessionEvent {
                timestamp: 2000,
                event_type: "assistant".to_string(),
                tool: None,
                content: "hi".to_string(),
            },
            SessionEvent {
                timestamp: 3000,
                event_type: "tool_call".to_string(),
                tool: Some("Bash".to_string()),
                content: "ls".to_string(),
            },
        ];

        let mut file = fs::File::create(&recording_path).unwrap();
        for event in &events {
            let mut line = serde_json::to_string(event).unwrap();
            line.push('\n');
            file.write_all(line.as_bytes()).unwrap();
        }

        // Read back and verify
        let content = fs::read_to_string(&recording_path).unwrap();
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 3);

        let first: SessionEvent = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first.event_type, "user");
        assert_eq!(first.content, "hello");

        let third: SessionEvent = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(third.event_type, "tool_call");
        assert_eq!(third.tool.as_deref(), Some("Bash"));

        // Clean up (stop)
        fs::remove_file(&active_path).unwrap();
        assert!(!active_path.exists());
    }

    #[test]
    fn test_session_event_serialization() {
        let event = SessionEvent {
            timestamp: 1708848000000,
            event_type: "user".to_string(),
            tool: None,
            content: "hello world".to_string(),
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"user\""));
        assert!(!json.contains("\"tool\""));

        let event_with_tool = SessionEvent {
            timestamp: 1708848000000,
            event_type: "tool_call".to_string(),
            tool: Some("Bash".to_string()),
            content: "ls -la".to_string(),
        };

        let json = serde_json::to_string(&event_with_tool).unwrap();
        assert!(json.contains("\"tool\":\"Bash\""));
    }

    #[test]
    fn test_session_event_deserialization() {
        let json =
            r#"{"timestamp":1708848000000,"type":"tool_call","tool":"Read","content":"file.rs"}"#;
        let event: SessionEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event_type, "tool_call");
        assert_eq!(event.tool.as_deref(), Some("Read"));
        assert_eq!(event.content, "file.rs");
    }

    #[test]
    fn test_jsonl_append_format() {
        let events = vec![
            SessionEvent {
                timestamp: 1000,
                event_type: "user".to_string(),
                tool: None,
                content: "first".to_string(),
            },
            SessionEvent {
                timestamp: 2000,
                event_type: "assistant".to_string(),
                tool: None,
                content: "second".to_string(),
            },
        ];

        let mut buf = String::new();
        for event in &events {
            let mut line = serde_json::to_string(event).unwrap();
            line.push('\n');
            buf.push_str(&line);
        }

        let lines: Vec<&str> = buf.lines().collect();
        assert_eq!(lines.len(), 2);

        // Each line should be valid JSON
        for line in &lines {
            let _: SessionEvent = serde_json::from_str(line).unwrap();
        }
    }

    #[test]
    fn test_recording_info_sorting() {
        use std::time::{Duration, SystemTime};

        let mut recordings = [
            RecordingInfo {
                id: "old".to_string(),
                event_count: 5,
                file_size: 100,
                modified: SystemTime::UNIX_EPOCH + Duration::from_secs(1000),
                meta: None,
            },
            RecordingInfo {
                id: "new".to_string(),
                event_count: 10,
                file_size: 200,
                modified: SystemTime::UNIX_EPOCH + Duration::from_secs(2000),
                meta: None,
            },
        ];

        recordings.sort_by_key(|r| std::cmp::Reverse(r.modified));
        assert_eq!(recordings[0].id, "new");
        assert_eq!(recordings[1].id, "old");
    }

    // ─── should_skip tests ─────────────────────────────────────────

    #[test]
    fn test_should_skip_empty_assistant() {
        assert!(should_skip("assistant", ""));
        assert!(should_skip("assistant", "   "));
    }

    #[test]
    fn test_should_skip_stop_markers() {
        assert!(should_skip("assistant", "[stop: turn_end]"));
        assert!(should_skip("assistant", "[stop: end_turn]"));
        assert!(should_skip("assistant", "[stop: something_else]"));
    }

    #[test]
    fn test_should_not_skip_real_assistant() {
        assert!(!should_skip("assistant", "Here is your answer"));
        assert!(!should_skip("assistant", "I'll help with that"));
    }

    #[test]
    fn test_should_skip_mur_session_tool_call() {
        let content = r#"{"command": "mur session start"}"#;
        assert!(should_skip("tool_call", content));

        let content = r#"{"command": "mur session stop"}"#;
        assert!(should_skip("tool_call", content));
    }

    #[test]
    fn test_should_not_skip_other_tool_calls() {
        let content = r#"{"command": "ls -la"}"#;
        assert!(!should_skip("tool_call", content));

        assert!(!should_skip("tool_call", "plain text"));
    }

    #[test]
    fn test_should_not_skip_user_events() {
        assert!(!should_skip("user", ""));
        assert!(!should_skip("user", "[stop: turn_end]"));
    }

    // ─── SessionMeta tests ─────────────────────────────────────────

    #[test]
    fn test_session_meta_serialization() {
        let meta = SessionMeta {
            id: "test-123".to_string(),
            source: "claude-code".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            stopped_at: None,
            title: Some("Hello world".to_string()),
            tools_used: vec!["Bash".to_string(), "Read".to_string()],
            user_turns: 3,
            assistant_turns: 4,
        };

        let json = serde_json::to_string_pretty(&meta).unwrap();
        let parsed: SessionMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "test-123");
        assert_eq!(parsed.source, "claude-code");
        assert_eq!(parsed.title, Some("Hello world".to_string()));
        assert_eq!(parsed.tools_used.len(), 2);
        assert_eq!(parsed.user_turns, 3);
        assert_eq!(parsed.assistant_turns, 4);
        assert!(parsed.stopped_at.is_none());
    }

    #[test]
    fn test_session_meta_file_operations() {
        let tmp = tempfile::TempDir::new().unwrap();
        let meta_dir = tmp.path().join("meta_test");
        fs::create_dir_all(&meta_dir).unwrap();

        let meta = SessionMeta {
            id: "abc-456".to_string(),
            source: "test".to_string(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            stopped_at: None,
            title: None,
            tools_used: vec![],
            user_turns: 0,
            assistant_turns: 0,
        };

        // Write and read back
        let path = meta_dir.join("abc-456.meta.json");
        let json = serde_json::to_string_pretty(&meta).unwrap();
        fs::write(&path, &json).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        let parsed: SessionMeta = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.id, "abc-456");
        assert!(parsed.title.is_none());
        assert_eq!(parsed.user_turns, 0);

        // Simulate title update
        let mut updated = parsed;
        updated.title = Some("My first session".to_string());
        updated.user_turns = 2;
        updated.tools_used = vec!["Bash".to_string()];

        let json = serde_json::to_string_pretty(&updated).unwrap();
        fs::write(&path, &json).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        let final_meta: SessionMeta = serde_json::from_str(&content).unwrap();
        assert_eq!(final_meta.title, Some("My first session".to_string()));
        assert_eq!(final_meta.user_turns, 2);
        assert_eq!(final_meta.tools_used, vec!["Bash".to_string()]);
    }

    #[test]
    fn test_title_truncation() {
        let long_content = "a".repeat(200);
        let title: String = long_content.chars().take(80).collect();
        assert_eq!(title.len(), 80);
    }

    #[test]
    fn test_tools_used_deduplication() {
        let mut tools: BTreeSet<String> = BTreeSet::new();
        tools.insert("Bash".to_string());
        tools.insert("Read".to_string());
        tools.insert("Bash".to_string()); // duplicate
        assert_eq!(tools.len(), 2);
    }
}
