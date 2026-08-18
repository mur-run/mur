use anyhow::Result;
use std::io::{BufRead, Write};
use std::path::Path;

use super::event::NormalizedEvent;

#[allow(dead_code)] // called from cmd::hook in Task 4
pub fn enqueue(event: &NormalizedEvent) -> Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    let queue_dir = home.join(".mur").join("queue");
    std::fs::create_dir_all(&queue_dir)?;
    enqueue_to(event, &queue_dir.join("events.jsonl"))
}

pub fn enqueue_to(event: &NormalizedEvent, path: &Path) -> Result<()> {
    // Redact before the line reaches disk (#979). This file records shell
    // command lines verbatim, so anything ever typed on one — an API key in an
    // argument, an Authorization header, a token in an env assignment — landed
    // here in plain text. B0 rule 9's "telemetry sink redaction" covers the
    // runtime's own writer and never saw this one.
    //
    // Walk the JSON rather than regexing the serialised line: a replacement
    // then lands inside a string and cannot break the structure around it.
    // Same shape the telemetry writer has always used, now shared.
    let mut value = serde_json::to_value(event)?;
    mur_common::redact::redact_value(&mut value);
    // Stamp WHEN this was recorded (#979). Deliberately added here rather than
    // as a `NormalizedEvent` field: the event models what the hook reported,
    // the time models when the queue wrote it — different owners. Keeping it
    // out of the struct also leaves the 23 literal constructions alone.
    //
    // Without it nothing in 454,874 records knows when it happened, so
    // rotation cannot be age-based and `mur hook stats` cannot say what period
    // it covers. Stamped after redaction, so it can never itself be redacted.
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "recorded_at".to_string(),
            serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
        );
    }
    let line = serde_json::to_string(&value)? + "\n";
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(line.as_bytes())?;
    Ok(())
}

/// Read all events from the queue file. Returns an empty Vec if the file
/// does not exist or cannot be read. Malformed JSON lines are silently skipped
/// so a corrupt line never crashes the stats command.
#[allow(dead_code)] // called from cmd::hook_stats in Task 3
pub fn read_all_events(path: &Path) -> Vec<NormalizedEvent> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    std::io::BufReader::new(file)
        .lines()
        .filter_map(|line| {
            let l = line.ok()?;
            let trimmed = l.trim();
            if trimmed.is_empty() {
                return None;
            }
            serde_json::from_str(trimmed).ok()
        })
        .collect()
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
            transcript_path: None,
            tool_response: None,
            cwd: None,
            duration_ms: None,
            is_duration_record: false,
        }
    }

    /// #979: this file records shell command lines verbatim, so a credential
    /// typed on one used to land here in plain text. The chokepoint is
    /// `enqueue_to` — every producer goes through it, so nothing has to
    /// remember to redact.
    #[test]
    fn a_secret_in_a_command_line_never_reaches_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");

        let mut ev = make_event("hello");
        ev.tool_called = Some("Bash".into());
        ev.tool_input = Some(serde_json::json!({
            "command": "curl -H 'Authorization: Bearer x' https://api.example.com",
            "key": "sk-ant-abcdefghij0123456789klmnopqrst",
        }));
        enqueue_to(&ev, &path).unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            !written.contains("sk-ant-abcdefghij0123456789klmnopqrst"),
            "the API key reached disk:\n{written}"
        );
        assert!(
            written.contains("[REDACTED:"),
            "nothing was redacted at all:\n{written}"
        );
        // Still a valid JSON line — redacting a string leaf must not break the
        // structure around it, which is why this walks the value rather than
        // regexing the serialised line.
        let parsed: serde_json::Value = serde_json::from_str(written.trim()).unwrap();
        assert_eq!(parsed["tool_called"], "Bash");
    }

    /// #979: an append-only log whose records do not know when they happened
    /// cannot be rotated by age, and its stats command cannot say what period
    /// it covers. Stamped by the WRITER, after redaction so the timestamp
    /// itself can never be redacted away.
    #[test]
    fn every_written_record_carries_when_it_was_recorded() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        enqueue_to(&make_event("hello"), &path).unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(written.trim()).unwrap();
        let ts = v["recorded_at"].as_str().expect("no recorded_at");
        chrono::DateTime::parse_from_rfc3339(ts)
            .unwrap_or_else(|e| panic!("recorded_at is not RFC3339: {ts} ({e})"));
    }

    /// The reader must not care that the writer adds a field it does not model
    /// — otherwise stamping the record would break every existing consumer.
    #[test]
    fn the_reader_still_parses_a_stamped_record() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        enqueue_to(&make_event("hi"), &path).unwrap();

        let back = read_all_events(&path);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].query.as_deref(), Some("hi"));
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

    #[test]
    fn read_all_events_returns_empty_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.jsonl");
        let events = read_all_events(&path);
        assert!(events.is_empty());
    }

    #[test]
    fn read_all_events_parses_written_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        enqueue_to(&make_event("first"), &path).unwrap();
        enqueue_to(&make_event("second"), &path).unwrap();
        let events = read_all_events(&path);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].query.as_deref(), Some("first"));
        assert_eq!(events[1].query.as_deref(), Some("second"));
    }

    #[test]
    fn read_all_events_skips_malformed_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(&path, "not-json\n{\"bad\":true}\n").unwrap();
        let events = read_all_events(&path);
        assert!(
            events.is_empty(),
            "malformed lines must be silently skipped"
        );
    }

    #[test]
    fn read_all_events_returns_empty_for_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(&path, "").unwrap();
        assert!(read_all_events(&path).is_empty());
    }

    #[test]
    fn read_all_events_skips_blank_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(&path, "\n\n  \n").unwrap();
        assert!(read_all_events(&path).is_empty());
    }
}
