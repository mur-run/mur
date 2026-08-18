use anyhow::Result;
use std::io::{BufRead, Write};
use std::path::Path;

use super::event::NormalizedEvent;

#[allow(dead_code)] // called from cmd::hook in Task 4
pub fn enqueue(event: &NormalizedEvent) -> Result<()> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    let mur_home = home.join(".mur");
    let queue_dir = mur_home.join("queue");
    std::fs::create_dir_all(&queue_dir)?;
    let path = queue_dir.join("events.jsonl");

    // Checked here rather than inside `enqueue_to` so the pure write path stays
    // testable without a config, and so rotation reads config exactly once per
    // append instead of once per test fixture.
    let cfg = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml"));
    // A failed rotation must not lose the event that triggered it: warn and
    // append anyway. The file being too large is a worse day than the file
    // being too large AND missing this record.
    if let Err(error) = rotate_if_needed(
        &path,
        cfg.capture.rotate_at_mb,
        cfg.capture.keep_generations,
    ) {
        tracing::warn!(%error, "capture queue rotation failed; appending anyway");
    }

    enqueue_to(event, &path)
}

/// Rotate the queue when it outgrows its budget, in the shape `newsyslog(8)`
/// uses: `events.jsonl` → `.0`, `.0` → `.1.gz`, oldest dropped.
///
/// No signal to any writer is needed, and that is not an accident of this
/// implementation — `enqueue_to` opens with `O_APPEND`, writes, and closes
/// within the call, so nothing holds a descriptor across a rename. A process
/// that is mid-append keeps writing to the renamed inode, so its line lands in
/// `.0` rather than being lost.
///
/// Two processes deciding to rotate at once WOULD race, so the decision is
/// taken under a sidecar lock — the same idiom `mur-channel` uses for its
/// event log, and for the same reason.
///
/// Compression does not make the old records safe, only smaller: a rotated
/// generation still holds whatever was written before redaction existed.
/// `~/.mur/queue` is denied to agents wholesale (#978), which covers the
/// generations too — but a backup of `~/.mur` carries them.
fn rotate_if_needed(path: &Path, rotate_at_mb: u64, keep: u32) -> Result<()> {
    use std::io::Write as _;

    let len = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(_) => return Ok(()),
    };
    if len < rotate_at_mb.saturating_mul(1024 * 1024) {
        return Ok(());
    }

    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let lock_path = dir.join("rotate.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    fs2::FileExt::lock_exclusive(&lock)?;

    // Re-check under the lock: another process may have rotated while we
    // waited, and rotating twice would throw away a generation for nothing.
    let still_big = std::fs::metadata(path)
        .map(|m| m.len() >= rotate_at_mb.saturating_mul(1024 * 1024))
        .unwrap_or(false);
    if !still_big {
        fs2::FileExt::unlock(&lock).ok();
        return Ok(());
    }

    let gen_path = |n: u32| -> std::path::PathBuf {
        if n == 0 {
            path.with_extension("jsonl.0")
        } else {
            path.with_extension(format!("jsonl.{n}.gz"))
        }
    };

    // Drop the oldest, then shift each generation up one.
    let _ = std::fs::remove_file(gen_path(keep));
    for n in (1..keep).rev() {
        let _ = std::fs::rename(gen_path(n), gen_path(n + 1));
    }
    // `.0` is uncompressed (newsyslog keeps the newest that way); compress it
    // as it becomes `.1.gz`.
    if gen_path(0).exists() && keep >= 1 {
        let raw = std::fs::read(gen_path(0))?;
        let f = std::fs::File::create(gen_path(1))?;
        let mut enc = flate2::write::GzEncoder::new(f, flate2::Compression::default());
        enc.write_all(&raw)?;
        enc.finish()?;
        let _ = std::fs::remove_file(gen_path(0));
    }
    std::fs::rename(path, gen_path(0))?;

    fs2::FileExt::unlock(&lock).ok();
    Ok(())
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

/// One line of the queue: the event the hook reported, plus the metadata the
/// queue itself added when it wrote the line.
///
/// `recorded_at` is `None` for records written before stamping existed (#982).
/// Absence means "before stamping", never "error" — treating it as an error
/// would make the first report after that change cover nothing.
#[derive(Debug, Clone)]
pub struct QueueRecord {
    pub recorded_at: Option<chrono::DateTime<chrono::Utc>>,
    pub event: NormalizedEvent,
}

/// Read the queue as records, keeping the write-time metadata that
/// `read_all_events` drops. Malformed lines are skipped, as there.
pub fn read_all_records(path: &Path) -> Vec<QueueRecord> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
            let recorded_at = value
                .get("recorded_at")
                .and_then(|v| v.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&chrono::Utc));
            let event: NormalizedEvent = serde_json::from_value(value).ok()?;
            Some(QueueRecord { recorded_at, event })
        })
        .collect()
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

    /// The newsyslog shape: past the threshold the live file becomes `.0` and
    /// a fresh one starts. Nothing is signalled, because nothing holds a
    /// descriptor across the rename.
    #[test]
    fn rotation_moves_the_live_file_aside_and_starts_a_new_one() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        std::fs::write(&path, vec![b'x'; 2 * 1024 * 1024]).unwrap();

        rotate_if_needed(&path, 1, 5).unwrap();

        assert!(!path.exists(), "the live file should have been moved aside");
        assert!(path.with_extension("jsonl.0").exists(), "no .0 generation");

        // And the writer just carries on, creating a fresh file.
        enqueue_to(&make_event("after"), &path).unwrap();
        assert_eq!(read_all_events(&path).len(), 1);
    }

    /// Under the threshold nothing happens — rotation must not be a surprise
    /// that fires on an ordinary append.
    #[test]
    fn a_small_queue_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        enqueue_to(&make_event("small"), &path).unwrap();

        rotate_if_needed(&path, 64, 5).unwrap();

        assert!(path.exists());
        assert!(!path.with_extension("jsonl.0").exists());
    }

    /// Generations shift up and the oldest is dropped, so total disk is
    /// bounded by `keep` — the property that makes "when do we delete the
    /// 934 MB" a policy rather than a decision.
    #[test]
    fn generations_shift_up_and_the_oldest_is_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");
        let big = vec![b'x'; 2 * 1024 * 1024];

        for _ in 0..4 {
            std::fs::write(&path, &big).unwrap();
            rotate_if_needed(&path, 1, 2).unwrap();
        }

        assert!(path.with_extension("jsonl.0").exists(), "no .0");
        assert!(
            path.with_extension("jsonl.1.gz").exists(),
            ".1 should be gzipped"
        );
        assert!(
            !path.with_extension("jsonl.3.gz").exists(),
            "kept more generations than configured"
        );
    }

    /// `.1` onward is gzipped, and the bytes must survive the round trip —
    /// a rotated generation nobody can read back is just a slower delete.
    #[test]
    fn a_compressed_generation_still_holds_its_records() {
        use std::io::Read as _;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("events.jsonl");

        enqueue_to(&make_event("keep-me"), &path).unwrap();
        let pad = vec![b'\n'; 2 * 1024 * 1024];
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&pad)
            .unwrap();

        rotate_if_needed(&path, 1, 3).unwrap(); // live -> .0
        std::fs::write(&path, vec![b'y'; 2 * 1024 * 1024]).unwrap();
        rotate_if_needed(&path, 1, 3).unwrap(); // .0 -> .1.gz

        let gz = std::fs::File::open(path.with_extension("jsonl.1.gz")).unwrap();
        let mut text = String::new();
        flate2::read::GzDecoder::new(gz)
            .read_to_string(&mut text)
            .unwrap();
        assert!(
            text.contains("keep-me"),
            "the record did not survive compression"
        );
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
