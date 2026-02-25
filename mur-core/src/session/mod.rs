//! Session recording for Claude Code hooks.
//!
//! Records session events to append-only JSONL files for later analysis.
//! State file: `~/.mur/session/active.json`
//! Recordings: `~/.mur/session/recordings/<session-id>.jsonl`

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
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
#[derive(Debug, Serialize, Deserialize)]
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

    fs::remove_file(&active)?;
    Ok(Some(id))
}

/// Record an event to the active session. Returns Ok(false) if no session is active.
pub fn record(event_type: &str, tool: Option<&str>, content: &str) -> Result<bool> {
    let active = active_path();
    if !active.exists() {
        return Ok(false);
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

        recordings.push(RecordingInfo {
            id,
            event_count,
            file_size,
            modified,
        });
    }

    // Sort by modified time, newest first
    recordings.sort_by(|a, b| b.modified.cmp(&a.modified));

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
        fs::write(&active_path, serde_json::to_string_pretty(&session).unwrap()).unwrap();

        // Verify it can be read back
        let content = fs::read_to_string(&active_path).unwrap();
        let parsed: ActiveSession = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.id, "abc-123");

        // Write events to JSONL
        let recording_path = recordings_dir.join("abc-123.jsonl");
        let events = vec![
            SessionEvent { timestamp: 1000, event_type: "user".to_string(), tool: None, content: "hello".to_string() },
            SessionEvent { timestamp: 2000, event_type: "assistant".to_string(), tool: None, content: "hi".to_string() },
            SessionEvent { timestamp: 3000, event_type: "tool_call".to_string(), tool: Some("Bash".to_string()), content: "ls".to_string() },
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
        let json = r#"{"timestamp":1708848000000,"type":"tool_call","tool":"Read","content":"file.rs"}"#;
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

        let mut recordings = vec![
            RecordingInfo {
                id: "old".to_string(),
                event_count: 5,
                file_size: 100,
                modified: SystemTime::UNIX_EPOCH + Duration::from_secs(1000),
            },
            RecordingInfo {
                id: "new".to_string(),
                event_count: 10,
                file_size: 200,
                modified: SystemTime::UNIX_EPOCH + Duration::from_secs(2000),
            },
        ];

        recordings.sort_by(|a, b| b.modified.cmp(&a.modified));
        assert_eq!(recordings[0].id, "new");
        assert_eq!(recordings[1].id, "old");
    }
}
