//! Three-guard retention cleanup (spec §4.6).
//!
//! Guards (all three must pass or delete is skipped):
//!   1. Age > `retention_days` since the raw/<date>/ was written
//!   2. `summary/<date>.md` exists (so we never orphan the conversation)
//!   3. Audit `Delete` entry records successfully BEFORE `rm_rf`
//!
//! Any failure in guards 2 or 3 skips the delete — conservative by design.
#![allow(dead_code)] // Phase 1: retention_days_from_config wired in by later CLI tasks.

use anyhow::Result;
use chrono::{DateTime, Utc};
use std::fs;
use tracing::warn;

use super::audit::{Audit, AuditAction};
use super::paths::summary_paths_for;
use super::store::list_raw_dirs;

#[derive(Debug, Default)]
pub struct CleanupReport {
    pub dirs_scanned: u64,
    pub dirs_deleted: u64,
    pub dirs_skipped_not_old_enough: u64,
    pub dirs_skipped_no_summary: u64,
    pub dirs_errored: u64,
    pub bytes_freed: u64,
}

pub fn cleanup(
    now: DateTime<Utc>,
    retention_days: u32,
    root_override: Option<&str>,
) -> Result<CleanupReport> {
    let mut report = CleanupReport::default();
    let audit = Audit::open(root_override)?;

    for (date, dir) in list_raw_dirs(root_override)? {
        report.dirs_scanned += 1;

        // Guard 1: age
        let age_days = (now.date_naive() - date).num_days();
        if age_days < retention_days as i64 {
            report.dirs_skipped_not_old_enough += 1;
            continue;
        }

        // Guard 2: summary exists
        let (md, _yml) = summary_paths_for(date, root_override);
        if !md.exists() {
            warn!("retention: skipping {dir:?} — no summary at {md:?}");
            report.dirs_skipped_no_summary += 1;
            continue;
        }

        // Guard 3: compute bytes, record audit (P1: two-arg append + bytes_freed field), then remove
        let bytes = dir_size_bytes(&dir).unwrap_or(0);
        let action = AuditAction::Delete {
            target: dir.to_string_lossy().into_owned(),
            reason: format!("retention {retention_days}d"),
            bytes_freed: bytes,
        };
        if let Err(e) = audit.append(action, String::new()) {
            warn!("retention: audit append failed, skipping {dir:?}: {e:#}");
            report.dirs_errored += 1;
            continue;
        }
        if let Err(e) = fs::remove_dir_all(&dir) {
            warn!("retention: rm_rf failed for {dir:?}: {e:#}");
            report.dirs_errored += 1;
            continue;
        }
        report.dirs_deleted += 1;
        report.bytes_freed += bytes;
    }

    Ok(report)
}

fn dir_size_bytes(dir: &std::path::Path) -> std::io::Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            total += dir_size_bytes(&entry.path())?;
        } else {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

/// Read retention_days from `~/.mur/config.yaml` (`conversations.retention_days`).
/// Defaults to 30 if absent or unreadable.
pub fn retention_days_from_config() -> u32 {
    let Some(home) = dirs::home_dir() else {
        return 30;
    };
    let cfg = home.join(".mur").join("config.yaml");
    let Ok(text) = fs::read_to_string(&cfg) else {
        return 30;
    };
    let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(&text) else {
        return 30;
    };
    doc.get("conversations")
        .and_then(|c| c.get("retention_days"))
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or(30)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use mur_common::{Content, Message, Role, Source};

    fn seed_raw(root: &str, ymd: (i32, u32, u32)) {
        let ts = chrono::Utc
            .with_ymd_and_hms(ymd.0, ymd.1, ymd.2, 12, 0, 0)
            .unwrap();
        let msg = Message {
            v: 1,
            ts,
            src: Source::ClaudeCode,
            conv: "c".into(),
            role: Role::User,
            content: Content::Text { value: "x".into() },
            meta: serde_json::Value::Null,
            refs: vec![],
        };
        crate::conversations::store::append(&msg, Some(root)).unwrap();
    }

    fn write_summary(root: &str, ymd: (i32, u32, u32), body: &str) {
        let d = chrono::NaiveDate::from_ymd_opt(ymd.0, ymd.1, ymd.2).unwrap();
        let (md, _yml) = crate::conversations::paths::summary_paths_for(d, Some(root));
        std::fs::create_dir_all(md.parent().unwrap()).unwrap();
        std::fs::write(&md, body).unwrap();
    }

    #[test]
    fn old_raw_with_summary_is_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        seed_raw(root, (2026, 1, 1));
        write_summary(root, (2026, 1, 1), "---\ndate: 2026-01-01\n---\n");
        let now = chrono::Utc.with_ymd_and_hms(2026, 4, 19, 0, 0, 0).unwrap();
        let r = cleanup(now, 30, Some(root)).unwrap();
        assert_eq!(r.dirs_deleted, 1);
        let raw = crate::conversations::paths::raw_root(Some(root)).join("2026-01-01");
        assert!(!raw.exists());
    }

    #[test]
    fn old_raw_without_summary_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        seed_raw(root, (2026, 1, 1));
        let now = chrono::Utc.with_ymd_and_hms(2026, 4, 19, 0, 0, 0).unwrap();
        let r = cleanup(now, 30, Some(root)).unwrap();
        assert_eq!(r.dirs_deleted, 0);
        assert_eq!(r.dirs_skipped_no_summary, 1);
    }

    #[test]
    fn recent_raw_kept_even_with_summary() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        seed_raw(root, (2026, 4, 18));
        write_summary(root, (2026, 4, 18), "---\n---\n");
        let now = chrono::Utc.with_ymd_and_hms(2026, 4, 19, 0, 0, 0).unwrap();
        let r = cleanup(now, 30, Some(root)).unwrap();
        assert_eq!(r.dirs_deleted, 0);
    }

    #[test]
    fn audit_records_bytes_freed() {
        // P1-reconciled check: Delete entry carries bytes_freed per amendment.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        seed_raw(root, (2026, 1, 1));
        write_summary(root, (2026, 1, 1), "---\n---\n");
        let now = chrono::Utc.with_ymd_and_hms(2026, 4, 19, 0, 0, 0).unwrap();
        cleanup(now, 30, Some(root)).unwrap();
        let audit_path = crate::conversations::paths::audit_path(Some(root));
        let text = std::fs::read_to_string(&audit_path).unwrap();
        assert!(text.contains("\"kind\":\"delete\""));
        assert!(text.contains("\"bytes_freed\""));
    }
}
