//! Ambient session capture (spec 2026-06-11 §3.1).
//!
//! Hooks call [`capture`] on every prompt / post-tool / stop event. Events are
//! written to `~/.mur/session/recordings/<session_key>.jsonl` keyed by the hook
//! payload's session_id — no `active.json` gate, no agent compliance needed.
//! Secrets are scrubbed at write time; noise is filtered with `should_skip`.

use anyhow::Result;
use std::path::Path;

use super::{SessionEvent, should_skip};
use crate::inject::event::{EventKind, NormalizedEvent};

/// Marker file written by `mur in`: the next captured event marks its session.
pub(crate) const MARK_NEXT_FILE: &str = "mark-next";

/// Last assistant text from a Claude Code transcript is capped to this many
/// chars so recordings stay small.
const ASSISTANT_CAP_CHARS: usize = 2000;

/// Resolve the recording key for an event. Providers without session ids
/// (cursor, copilot) fall into a per-provider daily bucket.
pub fn session_key(ev: &NormalizedEvent) -> String {
    match ev.session_id.as_deref() {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => format!(
            "{}-ambient-{}",
            ev.tool_provider,
            chrono::Utc::now().format("%Y-%m-%d")
        ),
    }
}

/// Best-effort exit code from a claude PostToolUse `tool_response`.
fn exit_code_of(ev: &NormalizedEvent) -> Option<i32> {
    let resp = ev.tool_response.as_ref()?;
    resp.get("exit_code")
        .or_else(|| resp.get("exitCode"))
        .and_then(|v| v.as_i64())
        .map(|v| v as i32)
}

/// Read the current git branch from `<cwd>/.git/HEAD` without spawning git.
fn git_branch_of(cwd: Option<&str>) -> Option<String> {
    let head = Path::new(cwd?).join(".git").join("HEAD");
    let content = std::fs::read_to_string(head).ok()?;
    content
        .trim()
        .strip_prefix("ref: refs/heads/")
        .map(str::to_owned)
}

/// Map a hook event to a recordable SessionEvent. Returns None for kinds that
/// carry nothing recordable (e.g. SessionStart, empty prompts).
pub fn to_session_event(ev: &NormalizedEvent) -> Option<SessionEvent> {
    let (event_type, tool, content) = match ev.kind {
        EventKind::Prompt => {
            let q = ev.query.as_deref().unwrap_or("").trim();
            if q.is_empty() {
                return None;
            }
            ("user", None, q.to_string())
        }
        EventKind::Tool => {
            let name = ev.tool_called.clone()?;
            let input = ev
                .tool_input
                .as_ref()
                .map(|v| v.to_string())
                .unwrap_or_default();
            ("tool_call", Some(name), input)
        }
        EventKind::Stop => {
            let content = last_assistant_message(ev.transcript_path.as_deref())?;
            ("assistant", None, content)
        }
        EventKind::SessionStart => return None,
    };
    Some(SessionEvent {
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        event_type: event_type.to_string(),
        tool,
        content,
        working_dir: ev.cwd.clone(),
        git_branch: git_branch_of(ev.cwd.as_deref()),
        exit_code: exit_code_of(ev),
    })
}

fn last_assistant_message(transcript_path: Option<&str>) -> Option<String> {
    let content = std::fs::read_to_string(transcript_path?).ok()?;
    for line in content.lines().rev() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        // transcript schema: {type:"assistant", message:{content:[{type:"text",text:"…"}, …]}}
        let texts: Vec<String> = v
            .pointer("/message/content")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|b| {
                        (b.get("type").and_then(|t| t.as_str()) == Some("text"))
                            .then(|| b.get("text").and_then(|t| t.as_str()).unwrap_or(""))
                            .map(str::to_owned)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let joined = texts.join("\n");
        if !joined.trim().is_empty() {
            return Some(joined.chars().take(ASSISTANT_CAP_CHARS).collect());
        }
    }
    None
}

/// Capture into an explicit session dir (testable core). Returns true if an
/// event was written.
pub fn capture_in_dir(session_dir: &Path, ev: &NormalizedEvent) -> Result<bool> {
    let Some(mut event) = to_session_event(ev) else {
        return Ok(false);
    };
    if should_skip(&event.event_type, &event.content) {
        return Ok(false);
    }
    event = super::scrub::scrub_event(&event);

    let recordings = session_dir.join("recordings");
    let key = session_key(ev);
    super::record_event_in_dir(&recordings, &key, &ev.tool_provider, &event)?;

    // `mur in` wrote a mark-next marker: mark this session and consume it.
    let marker = session_dir.join(MARK_NEXT_FILE);
    if marker.exists() {
        let meta_path = recordings.join(format!("{}.meta.json", key));
        if let Ok(content) = std::fs::read_to_string(&meta_path)
            && let Ok(mut meta) = serde_json::from_str::<super::SessionMeta>(&content)
        {
            meta.marked = true;
            if let Ok(json) = serde_json::to_string_pretty(&meta) {
                let _ = std::fs::write(&meta_path, json);
            }
            let _ = std::fs::remove_file(&marker);
        }
    }
    Ok(true)
}

/// Hook entry point: honors `session.capture` config; never panics, never
/// blocks the hook on error (callers `let _ =` the result).
pub fn capture(ev: &NormalizedEvent) -> Result<bool> {
    let mode = crate::store::config::load_config()
        .map(|c| c.session.capture)
        .unwrap_or_else(|_| "ambient".to_string());
    if mode != "ambient" {
        // "manual" keeps the legacy active.json path; "off" records nothing.
        return Ok(false);
    }
    capture_in_dir(&crate::paths::mur_root(None).join("session"), ev)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inject::event::{EventKind, NormalizedEvent};

    fn ev(kind: EventKind) -> NormalizedEvent {
        NormalizedEvent {
            kind,
            tool_provider: "claude".to_string(),
            query: Some("deploy the api".to_string()),
            tool_called: Some("Bash".to_string()),
            tool_input: Some(serde_json::json!({"command": "fly deploy"})),
            stop_reason: None,
            session_id: Some("sess-amb-1".to_string()),
            transcript_path: None,
            tool_response: Some(serde_json::json!({"exit_code": 0})),
            cwd: None,
            duration_ms: None,
            is_duration_record: false,
        }
    }

    #[test]
    fn capture_prompt_and_tool_writes_keyed_recording() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sdir = tmp.path().join("session");

        assert!(capture_in_dir(&sdir, &ev(EventKind::Prompt)).unwrap());
        assert!(capture_in_dir(&sdir, &ev(EventKind::Tool)).unwrap());

        let rec = sdir.join("recordings").join("sess-amb-1.jsonl");
        let content = std::fs::read_to_string(&rec).unwrap();
        assert_eq!(content.lines().count(), 2);
        let last: crate::session::SessionEvent =
            serde_json::from_str(content.lines().last().unwrap()).unwrap();
        assert_eq!(last.event_type, "tool_call");
        assert_eq!(last.exit_code, Some(0));
    }

    #[test]
    fn session_key_falls_back_to_daily_bucket() {
        let mut e = ev(EventKind::Prompt);
        e.session_id = None;
        e.tool_provider = "cursor".to_string();
        let key = session_key(&e);
        assert!(key.starts_with("cursor-ambient-"));
    }

    #[test]
    fn mark_next_marker_marks_and_is_consumed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sdir = tmp.path().join("session");
        std::fs::create_dir_all(&sdir).unwrap();
        std::fs::write(sdir.join(MARK_NEXT_FILE), "").unwrap();

        capture_in_dir(&sdir, &ev(EventKind::Prompt)).unwrap();

        let meta: crate::session::SessionMeta = serde_json::from_str(
            &std::fs::read_to_string(sdir.join("recordings").join("sess-amb-1.meta.json")).unwrap(),
        )
        .unwrap();
        assert!(meta.marked);
        assert!(!sdir.join(MARK_NEXT_FILE).exists());
    }

    #[test]
    fn stop_without_transcript_records_nothing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sdir = tmp.path().join("session");
        let mut e = ev(EventKind::Stop);
        e.transcript_path = None;
        assert!(!capture_in_dir(&sdir, &e).unwrap());
    }

    #[test]
    fn stop_reads_last_assistant_from_transcript() {
        let tmp = tempfile::TempDir::new().unwrap();
        let transcript = tmp.path().join("t.jsonl");
        std::fs::write(
            &transcript,
            concat!(
                r#"{"type":"user","message":{"content":[{"type":"text","text":"hi"}]}}"#,
                "\n",
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"done: deployed"}]}}"#,
                "\n",
            ),
        )
        .unwrap();
        let sdir = tmp.path().join("session");
        let mut e = ev(EventKind::Stop);
        e.transcript_path = Some(transcript.to_string_lossy().to_string());
        assert!(capture_in_dir(&sdir, &e).unwrap());
        let content =
            std::fs::read_to_string(sdir.join("recordings").join("sess-amb-1.jsonl")).unwrap();
        assert!(content.contains("done: deployed"));
    }
}
