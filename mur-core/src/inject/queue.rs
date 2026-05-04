use anyhow::Result;
use std::io::Write;

use super::event::NormalizedEvent;

#[allow(dead_code)] // called from cmd::hook in Task 4
pub fn enqueue(event: &NormalizedEvent) -> Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    let queue_dir = home.join(".mur").join("queue");
    std::fs::create_dir_all(&queue_dir)?;
    let path = queue_dir.join("events.jsonl");
    let line = serde_json::to_string(event)? + "\n";
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    f.write_all(line.as_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inject::event::{EventKind, NormalizedEvent};
    use std::io::Write;

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
    fn written_line_is_valid_json() {
        let event = make_event("how do I use anyhow?");
        let line = serde_json::to_string(&event).unwrap() + "\n";
        let parsed: NormalizedEvent = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(parsed.kind, EventKind::Prompt);
        assert_eq!(parsed.query.as_deref(), Some("how do I use anyhow?"));
    }

    #[test]
    fn multiple_appends_produce_valid_jsonl() {
        // Write 3 events to a temp file and verify 3 parseable lines
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        for i in 0..3 {
            let event = make_event(&format!("query {i}"));
            let line = serde_json::to_string(&event).unwrap() + "\n";
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(line.as_bytes()).unwrap();
        }
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = content.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 3);
        for (i, line) in lines.iter().enumerate() {
            let ev: NormalizedEvent = serde_json::from_str(line).unwrap();
            assert_eq!(ev.query.as_deref(), Some(format!("query {i}").as_str()));
        }
    }
}
