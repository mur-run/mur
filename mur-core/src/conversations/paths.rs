//! Canonical path helpers for the conversations archive.
//!
//! All paths live under `$MUR_DIR/conversations/` where `$MUR_DIR`
//! defaults to `~/.mur`. Accepting an override string is only for tests.

use chrono::{DateTime, NaiveDate, Utc};
use mur_common::Source;
use std::path::PathBuf;

/// Default mur root (`~/.mur`) or the override path for tests.
pub fn mur_root(override_path: Option<&str>) -> PathBuf {
    if let Some(p) = override_path {
        return PathBuf::from(p);
    }
    dirs::home_dir().expect("no home dir").join(".mur")
}

pub fn conversations_root(override_path: Option<&str>) -> PathBuf {
    mur_root(override_path).join("conversations")
}

pub fn raw_root(override_path: Option<&str>) -> PathBuf {
    conversations_root(override_path).join("raw")
}

pub fn raw_dir_for(ts: DateTime<Utc>, override_path: Option<&str>) -> PathBuf {
    let date = ts.date_naive();
    raw_root(override_path).join(date.format("%Y-%m-%d").to_string())
}

pub fn raw_file_for(
    ts: DateTime<Utc>,
    src: Source,
    conv_id: &str,
    override_path: Option<&str>,
) -> PathBuf {
    raw_dir_for(ts, override_path).join(format!("{}_{}.jsonl", src.file_prefix(), conv_id))
}

pub fn summary_root(override_path: Option<&str>) -> PathBuf {
    conversations_root(override_path).join("summary")
}

pub fn summary_paths_for(date: NaiveDate, override_path: Option<&str>) -> (PathBuf, PathBuf) {
    let root = summary_root(override_path);
    let d = date.format("%Y-%m-%d").to_string();
    (root.join(format!("{d}.md")), root.join(format!("{d}.yaml")))
}

pub fn index_path(override_path: Option<&str>) -> PathBuf {
    conversations_root(override_path).join("index.lance")
}

pub fn audit_path(override_path: Option<&str>) -> PathBuf {
    conversations_root(override_path).join("audit.jsonl")
}

pub fn blob_root(override_path: Option<&str>) -> PathBuf {
    conversations_root(override_path).join("blob")
}

pub fn user_dir(user_id: &str, override_path: Option<&str>) -> PathBuf {
    conversations_root(override_path)
        .join("users")
        .join(user_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn root_path() {
        let root = conversations_root(Some("/tmp/murtest"));
        assert_eq!(root, std::path::PathBuf::from("/tmp/murtest/conversations"));
    }

    #[test]
    fn raw_dir_format() {
        let ts = chrono::Utc
            .with_ymd_and_hms(2026, 4, 19, 11, 30, 45)
            .unwrap();
        let d = raw_dir_for(ts, Some("/tmp/m"));
        assert_eq!(
            d,
            std::path::PathBuf::from("/tmp/m/conversations/raw/2026-04-19")
        );
    }

    #[test]
    fn raw_file_naming() {
        let ts = chrono::Utc.with_ymd_and_hms(2026, 4, 19, 0, 0, 0).unwrap();
        let f = raw_file_for(ts, mur_common::Source::ClaudeCode, "3a87", Some("/tmp/m"));
        assert_eq!(
            f,
            std::path::PathBuf::from("/tmp/m/conversations/raw/2026-04-19/cc_3a87.jsonl")
        );
    }

    #[test]
    fn summary_paths() {
        let d = chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();
        let (md, yml) = summary_paths_for(d, Some("/tmp/m"));
        assert_eq!(
            md,
            std::path::PathBuf::from("/tmp/m/conversations/summary/2026-04-19.md")
        );
        assert_eq!(
            yml,
            std::path::PathBuf::from("/tmp/m/conversations/summary/2026-04-19.yaml")
        );
    }
}
