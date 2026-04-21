//! Hash-chained audit log at ~/.mur/conversations/audit.jsonl.
//!
//! Each entry's `entry_hash = sha256(prev_hash \n canonical_json(action) \n content_sha256)`.
//!
//! # Relationship to commander's audit log (P1 amendment)
//!
//! Commander's `~/.mur/commander/audit.jsonl` is a SEPARATE chain with its own
//! (workflow-oriented) schema and hash algorithm (see
//! `mur-commander/crates/engine/src/audit.rs`). We do NOT continue that chain.
//!
//! During migration (see `conversations::migrate`), a single `AuditAction::Migrate`
//! entry is written whose `bridged_from_hash` field carries commander's last
//! `entry_hash` as an opaque cryptographic pointer. Both chains verify
//! independently, without algorithm coupling.
#![allow(dead_code)] // Phase 1: stubs wired in by later tasks (migrate, retrieve).

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::sync::Mutex;
use uuid::Uuid;

use super::paths::audit_path;

pub const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuditAction {
    Write {
        target: String,
        bytes: u64,
    },
    Summarize {
        date: String,
        model: String,
        duration_ms: u64,
    },
    Index {
        date: String,
        vectors_added: u64,
    },
    Delete {
        target: String,
        reason: String,
        bytes_freed: u64,
    },
    /// Cryptographic bridge from commander's pre-existing audit chain (P1 amendment).
    Migrate {
        from: String,
        to: String,
        count: u64,
        /// Commander's last `entry_hash` at the moment of migration.
        /// [`ZERO_HASH`] if no prior commander audit existed.
        bridged_from_hash: String,
        /// Absolute path to commander's audit file at migration time.
        bridged_source: String,
    },
    Rollback {
        from: String,
        to: String,
        count: u64,
    },
    Error {
        layer: String,
        reason: String,
    },
    Rollup {
        rollup_kind: String, // "week" | "month"
        window: String,      // "2026-W16" or "2026-04"
        model: String,
        duration_ms: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: Uuid,
    pub ts: DateTime<Utc>,
    pub action: AuditAction,
    /// SHA-256 of content associated with this action (e.g., the JSONL line
    /// just appended to `raw/<date>/*.jsonl` for `Write`). Empty string when
    /// the action has no content body (`Migrate`, `Rollback`, `Error`).
    pub content_sha256: String,
    pub prev_hash: String,
    pub entry_hash: String,
}

pub struct Audit {
    root_override: Option<String>,
    last_hash: Mutex<String>,
}

impl Audit {
    pub fn open(root_override: Option<&str>) -> Result<Self> {
        let root_override = root_override.map(|s| s.to_string());
        let last_hash = read_last_hash(root_override.as_deref())?;
        Ok(Self {
            root_override,
            last_hash: Mutex::new(last_hash),
        })
    }

    /// Append an action to the chain. `content_sha256` is the SHA-256 of any
    /// content body this action refers to (empty string when N/A).
    pub fn append(&self, action: AuditAction, content_sha256: String) -> Result<AuditEntry> {
        let mut guard = self.last_hash.lock().expect("audit mutex poisoned");
        let prev_hash = guard.clone();
        let entry_hash = compute_hash(&prev_hash, &action, &content_sha256);
        let entry = AuditEntry {
            id: Uuid::new_v4(),
            ts: Utc::now(),
            action,
            content_sha256,
            prev_hash,
            entry_hash: entry_hash.clone(),
        };
        let path = audit_path(self.root_override.as_deref());
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open audit {path:?}"))?;
        serde_json::to_writer(&mut f, &entry)?;
        writeln!(f)?;
        *guard = entry_hash;
        Ok(entry)
    }
}

fn read_last_hash(root_override: Option<&str>) -> Result<String> {
    let path = audit_path(root_override);
    if !path.exists() {
        return Ok(ZERO_HASH.to_string());
    }
    let f = fs::File::open(&path)?;
    let mut last = ZERO_HASH.to_string();
    for line in BufReader::new(f).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let e: AuditEntry = serde_json::from_str(&line)?;
        last = e.entry_hash;
    }
    Ok(last)
}

fn compute_hash(prev_hash: &str, action: &AuditAction, content_sha256: &str) -> String {
    let canonical = serde_json::to_string(action).expect("action must serialize");
    let mut h = Sha256::new();
    h.update(prev_hash.as_bytes());
    h.update(b"\n");
    h.update(canonical.as_bytes());
    h.update(b"\n");
    h.update(content_sha256.as_bytes());
    hex::encode(h.finalize())
}

/// Replay chain from disk and verify every entry_hash. True = chain intact.
pub fn verify(root_override: Option<&str>) -> Result<bool> {
    let path = audit_path(root_override);
    if !path.exists() {
        return Ok(true);
    }
    let f = fs::File::open(&path)?;
    let mut prev = ZERO_HASH.to_string();
    for line in BufReader::new(f).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let e: AuditEntry = serde_json::from_str(&line)?;
        if e.prev_hash != prev {
            return Ok(false);
        }
        let expected = compute_hash(&e.prev_hash, &e.action, &e.content_sha256);
        if expected != e.entry_hash {
            return Ok(false);
        }
        prev = e.entry_hash;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_starts_from_zero_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let a = Audit::open(Some(root)).unwrap();
        let e = a
            .append(
                AuditAction::Write {
                    target: "raw/2026-04-19/cc_abc.jsonl".into(),
                    bytes: 128,
                },
                String::new(),
            )
            .unwrap();
        assert_eq!(e.prev_hash, ZERO_HASH);
        assert_ne!(e.entry_hash, ZERO_HASH);
    }

    #[test]
    fn chain_links_consecutive_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let a = Audit::open(Some(root)).unwrap();
        let e1 = a
            .append(
                AuditAction::Write {
                    target: "x".into(),
                    bytes: 1,
                },
                String::new(),
            )
            .unwrap();
        let e2 = a
            .append(
                AuditAction::Write {
                    target: "y".into(),
                    bytes: 2,
                },
                String::new(),
            )
            .unwrap();
        assert_eq!(e2.prev_hash, e1.entry_hash);
    }

    #[test]
    fn verify_replays_chain() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let a = Audit::open(Some(root)).unwrap();
        a.append(
            AuditAction::Write {
                target: "x".into(),
                bytes: 1,
            },
            String::new(),
        )
        .unwrap();
        a.append(
            AuditAction::Delete {
                target: "y".into(),
                reason: "retention".into(),
                bytes_freed: 42,
            },
            String::new(),
        )
        .unwrap();
        assert!(verify(Some(root)).unwrap());
    }

    #[test]
    fn verify_detects_tamper() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let a = Audit::open(Some(root)).unwrap();
        a.append(
            AuditAction::Write {
                target: "x".into(),
                bytes: 1,
            },
            String::new(),
        )
        .unwrap();
        let p = crate::conversations::paths::audit_path(Some(root));
        let content = std::fs::read_to_string(&p).unwrap();
        let tampered = content.replace("\"bytes\":1", "\"bytes\":99");
        std::fs::write(&p, tampered).unwrap();
        assert!(!verify(Some(root)).unwrap());
    }

    #[test]
    fn migrate_action_serializes_bridge_fields() {
        // P1 amendment: Migrate carries an opaque pointer (bridged_from_hash)
        // back to commander's last entry_hash without coupling hash algorithms.
        let a = AuditAction::Migrate {
            from: "/a".into(),
            to: "/b".into(),
            count: 5,
            bridged_from_hash: "abc123".into(),
            bridged_source: "/a/audit.jsonl".into(),
        };
        let s = serde_json::to_string(&a).unwrap();
        assert!(s.contains("\"kind\":\"migrate\""));
        assert!(s.contains("\"bridged_from_hash\":\"abc123\""));
        assert!(s.contains("\"bridged_source\":\"/a/audit.jsonl\""));
        // Roundtrip test
        let back: AuditAction = serde_json::from_str(&s).unwrap();
        if let AuditAction::Migrate {
            bridged_from_hash, ..
        } = back
        {
            assert_eq!(bridged_from_hash, "abc123");
        } else {
            panic!("expected Migrate variant");
        }
    }

    #[test]
    fn rollup_action_serializes_with_renamed_kind_field() {
        use serde_json::json;
        let a = AuditAction::Rollup {
            rollup_kind: "week".into(),
            window: "2026-W16".into(),
            model: "qwen3:14b".into(),
            duration_ms: 1234,
        };
        let v = serde_json::to_value(&a).unwrap();
        // The serde tag is "kind"; our variant's kind field was renamed to
        // "rollup_kind" to avoid collision with the tag.
        assert_eq!(v["kind"], json!("rollup"));
        assert_eq!(v["rollup_kind"], json!("week"));
        assert_eq!(v["window"], json!("2026-W16"));
        // Round-trip
        let round: AuditAction = serde_json::from_value(v).unwrap();
        matches!(round, AuditAction::Rollup { .. });
    }
}
