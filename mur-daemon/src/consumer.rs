use anyhow::Result;
use mur_core::inject::event::NormalizedEvent;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub fn queue_path() -> PathBuf {
    dirs::home_dir()
        .expect("no home dir")
        .join(".mur")
        .join("queue")
        .join("events.jsonl")
}

pub fn offset_path() -> PathBuf {
    dirs::home_dir()
        .expect("no home dir")
        .join(".mur")
        .join("queue")
        .join("offset")
}

/// Read byte offset from disk; returns 0 if file missing.
pub fn read_offset(path: &Path) -> u64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// Persist current byte offset to disk.
pub fn write_offset(path: &Path, offset: u64) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(path, offset.to_string())?;
    Ok(())
}

/// Read all new events from `queue_file` starting at `start_offset`.
/// Returns (new_events, new_offset).
pub fn drain_new(queue_file: &Path, start_offset: u64) -> Result<(Vec<NormalizedEvent>, u64)> {
    if !queue_file.exists() {
        return Ok((vec![], start_offset));
    }
    let mut f = std::fs::File::open(queue_file)?;
    f.seek(SeekFrom::Start(start_offset))?;

    let mut events = Vec::new();
    let mut new_offset = start_offset;
    let reader = BufReader::new(&f);

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            new_offset += line.len() as u64 + 1;
            continue;
        }
        new_offset += line.len() as u64 + 1; // +1 for newline
        if let Ok(ev) = serde_json::from_str::<NormalizedEvent>(trimmed) {
            events.push(ev);
        }
    }

    Ok((events, new_offset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_core::inject::event::{EventKind, NormalizedEvent};

    fn write_events(path: &Path, events: &[NormalizedEvent]) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        for ev in events {
            writeln!(f, "{}", serde_json::to_string(ev).unwrap()).unwrap();
        }
    }

    fn make_event(query: &str) -> NormalizedEvent {
        NormalizedEvent {
            kind: EventKind::Prompt,
            tool_provider: "claude".into(),
            query: Some(query.into()),
            tool_called: None,
            tool_input: None,
            stop_reason: None,
            session_id: Some("sess1".into()),
            duration_ms: None,
            is_duration_record: false,
        }
    }

    #[test]
    fn drain_reads_all_from_offset_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        write_events(&path, &[make_event("q1"), make_event("q2")]);
        let (events, new_off) = drain_new(&path, 0).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].query.as_deref(), Some("q1"));
        assert!(new_off > 0);
    }

    #[test]
    fn drain_reads_only_new_events_from_saved_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        write_events(&path, &[make_event("old")]);
        let (_, off1) = drain_new(&path, 0).unwrap();
        write_events(&path, &[make_event("new")]);
        let (events, _) = drain_new(&path, off1).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].query.as_deref(), Some("new"));
    }

    #[test]
    fn drain_missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.jsonl");
        let (events, off) = drain_new(&path, 0).unwrap();
        assert!(events.is_empty());
        assert_eq!(off, 0);
    }

    #[test]
    fn offset_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("offset");
        write_offset(&path, 12345).unwrap();
        assert_eq!(read_offset(&path), 12345);
    }

    #[test]
    fn read_offset_missing_returns_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing");
        assert_eq!(read_offset(&path), 0);
    }
}
