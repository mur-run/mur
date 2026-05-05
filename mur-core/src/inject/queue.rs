use anyhow::Result;
use std::io::Write;
use std::path::Path;

use super::event::NormalizedEvent;

#[allow(dead_code)] // called from cmd::hook in Task 4
pub fn enqueue(event: &NormalizedEvent) -> Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    let queue_dir = home.join(".mur").join("queue");
    std::fs::create_dir_all(&queue_dir)?;
    enqueue_to(event, &queue_dir.join("events.jsonl"))
}

fn enqueue_to(event: &NormalizedEvent, path: &Path) -> Result<()> {
    let line = serde_json::to_string(event)? + "\n";
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(line.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inject::event::{EventKind, NormalizedEvent};

    fn make_event(query: &str) -> NormalizedEvent {
        NormalizedEvent {
            kind: EventKind::Prompt,
            tool_provider: "claude".into(),
            query: Some(query.into()),
            tool_called: None,
            tool_input: None,
            stop_reason: None,
            session_id: Some("test_sess".into()),
        }
    }

    #[test]
    fn enqueue_to_writes_valid_json_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let event = make_event("how do I use anyhow?");
        enqueue_to(&event, &path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: NormalizedEvent = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(parsed.kind, EventKind::Prompt);
        assert_eq!(parsed.query.as_deref(), Some("how do I use anyhow?"));
    }

    #[test]
    fn enqueue_to_appends_multiple_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        for i in 0..3 {
            let event = make_event(&format!("query {i}"));
            enqueue_to(&event, &path).unwrap();
        }
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 3);
        for (i, line) in lines.iter().enumerate() {
            let ev: NormalizedEvent = serde_json::from_str(line).unwrap();
            assert_eq!(ev.query.as_deref(), Some(format!("query {i}").as_str()));
        }
    }

    #[test]
    fn enqueue_to_uses_append_not_truncate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        // Write first event
        enqueue_to(&make_event("first"), &path).unwrap();
        // Write second event — must not overwrite the first
        enqueue_to(&make_event("second"), &path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 2, "both events must be present (append mode)");
        let ev0: NormalizedEvent = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(ev0.query.as_deref(), Some("first"));
    }
}
