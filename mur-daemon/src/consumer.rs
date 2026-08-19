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

    // The queue rotates (mur-run/mur#983): past a size cap the live file is
    // renamed to `.0` and a fresh one starts at zero bytes. A stored offset
    // then points past the end of the new file — and seeking past EOF is
    // LEGAL, so the read simply returns nothing. The daemon would consume
    // silently forever, with no error to notice.
    //
    // A file shorter than the offset we hold means it was rotated or
    // truncated under us; either way the only correct position is the start.
    // This also covers a manual `> events.jsonl`.
    let len = f.metadata()?.len();
    let start_offset = if len < start_offset {
        tracing::info!(
            stored_offset = start_offset,
            file_len = len,
            "capture queue is shorter than the stored offset — rotated or truncated; resuming from the start"
        );
        0
    } else {
        start_offset
    };

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
            transcript_path: None,
            tool_response: None,
            cwd: None,
            duration_ms: None,
            is_duration_record: false,
        }
    }

    /// #983 rotates the queue: the live file is renamed away and a fresh one
    /// starts at zero. A stored offset then points past the end — and seeking
    /// past EOF is LEGAL, so without this the daemon reads nothing, forever,
    /// with no error anywhere. It would look exactly like an idle machine.
    #[test]
    fn a_rotated_queue_is_read_from_the_start_not_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let q = dir.path().join("events.jsonl");

        // A large "pre-rotation" file, consumed to its end.
        write_events(&q, &[make_event("old-1"), make_event("old-2")]);
        let (_first, offset) = drain_new(&q, 0).unwrap();
        assert!(offset > 0, "nothing was consumed to establish an offset");

        // Rotation: the live file is replaced by a fresh, shorter one.
        std::fs::remove_file(&q).unwrap();
        write_events(&q, &[make_event("after-rotation")]);

        let (events, new_offset) = drain_new(&q, offset).unwrap();
        assert_eq!(
            events.len(),
            1,
            "the post-rotation record was skipped — this is the silent-forever case"
        );
        assert_eq!(events[0].query.as_deref(), Some("after-rotation"));
        assert!(new_offset > 0 && new_offset <= std::fs::metadata(&q).unwrap().len());
    }

    /// And an unrotated queue must still resume where it left off — resetting
    /// on every call would re-consume the whole file each tick.
    #[test]
    fn an_unrotated_queue_still_resumes_from_the_offset() {
        let dir = tempfile::tempdir().unwrap();
        let q = dir.path().join("events.jsonl");

        write_events(&q, &[make_event("one")]);
        let (_a, offset) = drain_new(&q, 0).unwrap();

        write_events(&q, &[make_event("two")]);
        let (events, _) = drain_new(&q, offset).unwrap();

        assert_eq!(events.len(), 1, "resumed from the wrong place");
        assert_eq!(events[0].query.as_deref(), Some("two"));
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
