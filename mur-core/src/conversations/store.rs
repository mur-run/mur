//! Raw JSONL append-only writer/reader.
//!
//! - Each line is a serialized `mur_common::Message`.
//! - Writes open with `O_APPEND`; no file rewrites (spec §12 constraint #2).
//! - Reads walk every JSONL file for a date, sort by `ts`.
#![allow(dead_code)] // Phase 1: stubs wired in by later tasks (migrate, retrieve).

use anyhow::{Context, Result};
use chrono::NaiveDate;
use mur_common::Message;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use super::paths::{raw_dir_for, raw_file_for};

/// Append one message to its dated JSONL file. Creates parent dirs as needed.
pub fn append(msg: &Message, root_override: Option<&str>) -> Result<()> {
    let file_path = raw_file_for(msg.ts, msg.src, &msg.conv, root_override);
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir -p {parent:?}"))?;
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file_path)
        .with_context(|| format!("opening {file_path:?} for append"))?;
    serde_json::to_writer(&mut f, msg).context("serializing message")?;
    writeln!(f).context("writeln")?;
    Ok(())
}

/// Read every message recorded on `date`, across all sources, sorted by ts.
pub fn read_day(date: NaiveDate, root_override: Option<&str>) -> Result<Vec<Message>> {
    let ts = date.and_hms_opt(0, 0, 0).unwrap().and_utc();
    let dir = raw_dir_for(ts, root_override);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir).with_context(|| format!("readdir {dir:?}"))? {
        let entry = entry?;
        if entry.path().extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let f = fs::File::open(entry.path())?;
        for line in BufReader::new(f).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let m: Message =
                serde_json::from_str(&line).with_context(|| format!("parse {:?}", entry.path()))?;
            out.push(m);
        }
    }
    out.sort_by_key(|m| m.ts);
    Ok(out)
}

/// List dated raw directories (`YYYY-MM-DD` subdirs under `raw/`).
pub fn list_raw_dirs(root_override: Option<&str>) -> Result<Vec<(NaiveDate, PathBuf)>> {
    let raw = super::paths::raw_root(root_override);
    if !raw.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&raw)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Ok(date) = NaiveDate::parse_from_str(&name, "%Y-%m-%d") {
            out.push((date, entry.path()));
        }
    }
    out.sort_by_key(|(d, _)| *d);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use mur_common::{Content, Message, Role, Source};

    fn sample(ts: chrono::DateTime<chrono::Utc>, v: &str) -> Message {
        Message {
            v: 1,
            ts,
            src: Source::ClaudeCode,
            conv: "test-conv".into(),
            role: Role::User,
            content: Content::Text { value: v.into() },
            meta: serde_json::Value::Null,
            refs: vec![],
        }
    }

    #[test]
    fn append_creates_file_and_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let ts = chrono::Utc.with_ymd_and_hms(2026, 4, 19, 10, 0, 0).unwrap();
        let msg = sample(ts, "hello");
        append(&msg, Some(root)).unwrap();
        let path = crate::conversations::paths::raw_file_for(
            ts,
            Source::ClaudeCode,
            "test-conv",
            Some(root),
        );
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.trim_end().ends_with("}"));
        assert!(content.contains("hello"));
    }

    #[test]
    fn append_is_idempotent_per_line() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let ts = chrono::Utc.with_ymd_and_hms(2026, 4, 19, 10, 0, 0).unwrap();
        append(&sample(ts, "a"), Some(root)).unwrap();
        append(&sample(ts, "b"), Some(root)).unwrap();
        let path = crate::conversations::paths::raw_file_for(
            ts,
            Source::ClaudeCode,
            "test-conv",
            Some(root),
        );
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = content.lines().collect();
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn read_day_returns_all_messages_sorted_by_ts() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let t1 = chrono::Utc.with_ymd_and_hms(2026, 4, 19, 9, 0, 0).unwrap();
        let t2 = chrono::Utc.with_ymd_and_hms(2026, 4, 19, 10, 0, 0).unwrap();
        append(&sample(t2, "later"), Some(root)).unwrap();
        append(&sample(t1, "earlier"), Some(root)).unwrap();
        let date = chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();
        let msgs = read_day(date, Some(root)).unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(msgs[0].ts < msgs[1].ts);
    }
}
