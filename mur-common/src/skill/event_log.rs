//! Per-skill append-only event log (`~/.mur/skills/<name>/events.jsonl`).
//! Each line is a JSON-serialized `SkillEvent`. Used by fleet-sync for
//! set-union merge of evolved usage state across devices.
//!
//! Also provides manifest conflict resolution via Last-Writer-Wins (LWW)
//! for fleet-sync: when two devices have divergent manifests, the one
//! with the later `updated_at` timestamp wins.

use crate::skill::manifest::Skill;
use crate::skill::stats::SkillStats;
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SkillEvent {
    Retrieval {
        ts: DateTime<Utc>,
        device_id: String,
    },
    Execution {
        ts: DateTime<Utc>,
        device_id: String,
        /// "success" | "failure"
        outcome: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<String>,
        // ── Run-ledger enrichment (workflow-engine v2 P2; all default so
        //    existing events.jsonl lines keep parsing and fleet-sync's
        //    dedup_key (ts+kind+device) is unaffected) ──
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        /// "workflow" (the skill is broken) | "env" (network/credentials/…).
        /// The Broken fast-path (P4) only triggers on "workflow" with
        /// confidence ≥ threshold.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env_class: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        confidence: Option<f64>,
        /// "manual" | "schedule" | "agent"
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger: Option<String>,
    },
    Dismissed {
        ts: DateTime<Utc>,
        device_id: String,
    },
    Superseded {
        ts: DateTime<Utc>,
        device_id: String,
    },
}

impl SkillEvent {
    /// Stable key for set-dedup: timestamp-micros + kind + device.
    pub fn dedup_key(&self) -> String {
        match self {
            Self::Retrieval { ts, device_id } => {
                format!("{}:retrieval:{}", ts.timestamp_micros(), device_id)
            }
            Self::Execution { ts, device_id, .. } => {
                format!("{}:execution:{}", ts.timestamp_micros(), device_id)
            }
            Self::Dismissed { ts, device_id } => {
                format!("{}:dismissed:{}", ts.timestamp_micros(), device_id)
            }
            Self::Superseded { ts, device_id } => {
                format!("{}:superseded:{}", ts.timestamp_micros(), device_id)
            }
        }
    }

    pub fn ts(&self) -> DateTime<Utc> {
        match self {
            Self::Retrieval { ts, .. }
            | Self::Execution { ts, .. }
            | Self::Dismissed { ts, .. }
            | Self::Superseded { ts, .. } => *ts,
        }
    }
}

pub fn event_log_path(mur_home: &Path, skill_name: &str) -> PathBuf {
    mur_home
        .join("skills")
        .join(skill_name)
        .join("events.jsonl")
}

pub fn append_event(path: &Path, event: &SkillEvent) -> Result<()> {
    use fs2::FileExt;
    use std::io::{Seek, SeekFrom};

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(event)?;
    // Open read+write (not `append(true)`) and take an exclusive flock,
    // seeking to end ourselves — matches multimodal::ledger::append.
    // `append(true)` alone doesn't request enough access for `LockFileEx`
    // on Windows, and without a lock concurrent writers can interleave
    // their `write()` syscalls and tear a line.
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    f.lock_exclusive()?;
    f.seek(SeekFrom::End(0))?;
    // One write() call for the whole line (content + newline) so a torn
    // write can't happen even if the lock were ever dropped.
    f.write_all(format!("{line}\n").as_bytes())?;
    f.unlock()?;
    Ok(())
}

pub fn read_events(path: &Path) -> Result<Vec<SkillEvent>> {
    match std::fs::read_to_string(path) {
        Ok(s) => parse_events_jsonl(&s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(anyhow::Error::from(e)),
    }
}

pub fn parse_events_jsonl(raw: &str) -> Result<Vec<SkillEvent>> {
    raw.lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).map_err(anyhow::Error::from))
        .collect()
}

/// Set-union of two event logs, deduped by `dedup_key`, sorted by timestamp.
/// Commutative and idempotent.
pub fn union_events(mut a: Vec<SkillEvent>, b: Vec<SkillEvent>) -> Vec<SkillEvent> {
    let seen: HashSet<String> = a.iter().map(|e| e.dedup_key()).collect();
    for event in b {
        if !seen.contains(&event.dedup_key()) {
            a.push(event);
        }
    }
    a.sort_by_key(|e| e.ts());
    a
}

/// Apply a slice of new events to an existing `SkillStats`, updating only
/// usage counters. Lifecycle state, pinned, and anchor_confidence are
/// preserved — they are managed by the lifecycle module, not by events.
pub fn apply_new_events_to_stats(stats: &mut SkillStats, new_events: &[SkillEvent]) {
    for event in new_events {
        match event {
            SkillEvent::Retrieval { ts, .. } => {
                stats.usage_count += 1;
                stats.last_used_at = Some(stats.last_used_at.map(|e| e.max(*ts)).unwrap_or(*ts));
            }
            SkillEvent::Execution { ts, outcome, .. } => {
                stats.usage_count += 1;
                stats.last_used_at = Some(stats.last_used_at.map(|e| e.max(*ts)).unwrap_or(*ts));
                if outcome == "success" {
                    stats.success_count += 1;
                    stats.last_success_at =
                        Some(stats.last_success_at.map(|e| e.max(*ts)).unwrap_or(*ts));
                    if stats.first_successful_use_at.is_none() {
                        stats.first_successful_use_at = Some(*ts);
                    }
                } else {
                    stats.failure_count += 1;
                }
            }
            SkillEvent::Dismissed { .. } | SkillEvent::Superseded { .. } => {}
        }
    }
}

/// Outcome of one workflow/skill run, recorded into the per-skill ledger.
pub struct RunRecord<'a> {
    /// true = success
    pub success: bool,
    pub duration_ms: Option<u64>,
    pub exit_code: Option<i32>,
    /// stderr (or combined output) of the failing step; used to classify
    /// workflow-vs-environment failure. Ignored on success.
    pub stderr: Option<&'a str>,
    /// Step id/description that failed, if any.
    pub failed_step: Option<String>,
    /// "manual" | "schedule" | "agent"
    pub trigger: &'a str,
    /// Explicit user override of the env classification
    /// (`mur run --env-class workflow|env`).
    pub env_class_override: Option<&'a str>,
}

/// Append one enriched Execution event for a completed run — the run-ledger
/// write path (workflow-engine v2 P2). Returns the event written.
pub fn record_run(
    mur_home: &Path,
    skill_name: &str,
    device_id: &str,
    rec: &RunRecord<'_>,
) -> Result<SkillEvent> {
    let (env_class, confidence) = if rec.success {
        (None, None)
    } else if let Some(forced) = rec.env_class_override {
        (Some(forced.to_string()), Some(1.0))
    } else {
        let c = crate::skill::env_class::classify_failure(rec.stderr.unwrap_or(""));
        (Some(c.class.to_string()), Some(c.confidence))
    };

    let event = SkillEvent::Execution {
        ts: Utc::now(),
        device_id: device_id.to_string(),
        outcome: if rec.success { "success" } else { "failure" }.to_string(),
        error: (!rec.success)
            .then(|| rec.stderr.map(|s| s.chars().take(500).collect()))
            .flatten(),
        step: rec.failed_step.clone(),
        duration_ms: rec.duration_ms,
        exit_code: rec.exit_code,
        env_class,
        confidence,
        trigger: Some(rec.trigger.to_string()),
    };
    append_event(&event_log_path(mur_home, skill_name), &event)?;
    Ok(event)
}

/// Resolve manifest conflict via Last-Writer-Wins (LWW).
/// Returns the winning skill and the reason (local_wins, remote_wins, or force_local).
pub fn resolve_manifest_lww(
    local: Skill,
    remote: Skill,
    force_local: bool,
) -> (Skill, &'static str) {
    if force_local {
        return (local, "force_local");
    }
    if remote.manifest.updated_at > local.manifest.updated_at {
        (remote, "remote_newer")
    } else {
        (local, "local_newer_or_equal")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn record_run_classifies_and_appends() {
        let tmp = tempdir().unwrap();
        let ev = record_run(
            tmp.path(),
            "deploy-api",
            "dev-a",
            &RunRecord {
                success: false,
                duration_ms: Some(1200),
                exit_code: Some(7),
                stderr: Some("curl: (7) Connection refused"),
                failed_step: Some("health-check".into()),
                trigger: "manual",
                env_class_override: None,
            },
        )
        .unwrap();
        match &ev {
            SkillEvent::Execution {
                env_class, trigger, ..
            } => {
                assert_eq!(env_class.as_deref(), Some("env"));
                assert_eq!(trigger.as_deref(), Some("manual"));
            }
            _ => panic!("wrong kind"),
        }
        let events = read_events(&event_log_path(tmp.path(), "deploy-api")).unwrap();
        assert_eq!(events.len(), 1);

        // Success run records no env_class.
        let ev2 = record_run(
            tmp.path(),
            "deploy-api",
            "dev-a",
            &RunRecord {
                success: true,
                duration_ms: Some(900),
                exit_code: Some(0),
                stderr: None,
                failed_step: None,
                trigger: "schedule",
                env_class_override: None,
            },
        )
        .unwrap();
        match &ev2 {
            SkillEvent::Execution {
                env_class, outcome, ..
            } => {
                assert!(env_class.is_none());
                assert_eq!(outcome, "success");
            }
            _ => panic!("wrong kind"),
        }
    }

    #[test]
    fn legacy_execution_line_parses_and_enriched_roundtrips() {
        // Pre-P2 line without the run-ledger fields must keep parsing.
        let legacy = r#"{"kind":"execution","ts":"2026-05-30T00:00:00Z","device_id":"d","outcome":"success"}"#;
        let ev: SkillEvent = serde_json::from_str(legacy).unwrap();
        match &ev {
            SkillEvent::Execution {
                duration_ms,
                env_class,
                ..
            } => {
                assert!(duration_ms.is_none());
                assert!(env_class.is_none());
            }
            _ => panic!("wrong kind"),
        }

        // Enriched event round-trips.
        let enriched = SkillEvent::Execution {
            ts: chrono::DateTime::from_timestamp(1_748_000_000, 0).unwrap(),
            device_id: "d".into(),
            outcome: "failure".into(),
            error: Some("boom".into()),
            step: Some("deploy".into()),
            duration_ms: Some(8421),
            exit_code: Some(1),
            env_class: Some("workflow".into()),
            confidence: Some(0.6),
            trigger: Some("manual".into()),
        };
        let line = serde_json::to_string(&enriched).unwrap();
        let back: SkillEvent = serde_json::from_str(&line).unwrap();
        assert_eq!(back, enriched);
        // dedup_key shape unchanged (ts+kind+device) — fleet-sync compatible.
        assert!(enriched.dedup_key().ends_with(":execution:d"));
    }

    fn device() -> String {
        "dev-a".into()
    }

    fn retrieval(ts_offset_secs: i64) -> SkillEvent {
        let base = chrono::DateTime::from_timestamp(1_748_000_000 + ts_offset_secs, 0).unwrap();
        SkillEvent::Retrieval {
            ts: base,
            device_id: device(),
        }
    }

    fn exec_ok(ts_offset_secs: i64) -> SkillEvent {
        let base = chrono::DateTime::from_timestamp(1_748_000_000 + ts_offset_secs, 0).unwrap();
        SkillEvent::Execution {
            ts: base,
            device_id: device(),
            outcome: "success".into(),
            error: None,
            step: None,
            duration_ms: None,
            exit_code: None,
            env_class: None,
            confidence: None,
            trigger: None,
        }
    }

    fn exec_fail(ts_offset_secs: i64) -> SkillEvent {
        let base = chrono::DateTime::from_timestamp(1_748_000_000 + ts_offset_secs, 0).unwrap();
        SkillEvent::Execution {
            ts: base,
            device_id: device(),
            outcome: "failure".into(),
            error: Some("oops".into()),
            step: None,
            duration_ms: None,
            exit_code: None,
            env_class: None,
            confidence: None,
            trigger: None,
        }
    }

    #[test]
    fn append_then_read_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        append_event(&path, &retrieval(0)).unwrap();
        append_event(&path, &exec_ok(1)).unwrap();
        let events = read_events(&path).unwrap();
        assert_eq!(events.len(), 2);
    }

    /// Regression test for torn/interleaved lines under concurrent writers.
    ///
    /// N threads each append M events to the same path with no external
    /// synchronization; `append_event` itself must serialize the writes via
    /// flock, otherwise two writers' `write()` syscalls can interleave and
    /// glue/tear a line. `parse_events_jsonl` uses `.collect::<Result<_>>()`,
    /// so a torn line makes the *whole* `read_events` call return `Err`
    /// rather than silently dropping one entry — either way it's a real
    /// failure, so we assert both that reading succeeds and that we get
    /// back exactly the expected count.
    ///
    /// Verified this reproduces against the old `append(true)` + `writeln!`
    /// implementation: reverting `append_event` to that shape and rerunning
    /// this test failed within a handful of runs with
    /// `called \`Result::unwrap()\` on an \`Err\` value: trailing characters
    /// at line 1 column 69` — a torn/glued line that no longer parses as
    /// JSON. It's a data race so it doesn't fail on literally every run,
    /// but it reproduces reliably enough (a handful of tries) to be
    /// confident it exercises the bug.
    #[test]
    fn concurrent_appends_produce_no_torn_lines() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.jsonl");

        const THREADS: i64 = 8;
        const PER_THREAD: i64 = 25;

        let handles: Vec<_> = (0..THREADS)
            .map(|t| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for i in 0..PER_THREAD {
                        let ts_offset = t * PER_THREAD + i;
                        append_event(&path, &retrieval(ts_offset)).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let events = read_events(&path).unwrap();
        assert_eq!(events.len(), (THREADS * PER_THREAD) as usize);
    }

    #[test]
    fn union_deduplicates_identical_events() {
        let a = vec![retrieval(0), exec_ok(1)];
        let b = vec![exec_ok(1), exec_fail(2)];
        let merged = union_events(a, b);
        assert_eq!(merged.len(), 3); // dedup exec_ok(1)
    }

    #[test]
    fn union_is_commutative() {
        let a = vec![retrieval(0), exec_ok(1)];
        let b = vec![exec_ok(1), exec_fail(2)];
        let ab = union_events(a.clone(), b.clone());
        let ba = union_events(b, a);
        let ab_keys: Vec<_> = ab.iter().map(|e| e.dedup_key()).collect();
        let ba_keys: Vec<_> = ba.iter().map(|e| e.dedup_key()).collect();
        assert_eq!(ab_keys, ba_keys);
    }

    #[test]
    fn apply_new_events_updates_counters() {
        use crate::skill::stats::SkillStats;
        use chrono::Utc;
        let mut stats = SkillStats::new("test-skill", "1.0.0", "digest", Utc::now());
        let events = vec![exec_ok(1), exec_fail(2), retrieval(3)];
        apply_new_events_to_stats(&mut stats, &events);
        assert_eq!(stats.usage_count, 3);
        assert_eq!(stats.success_count, 1);
        assert_eq!(stats.failure_count, 1);
        assert!(stats.last_success_at.is_some());
        assert!(stats.first_successful_use_at.is_some());
    }

    #[test]
    fn read_events_returns_empty_for_missing_file() {
        let dir = tempdir().unwrap();
        let events = read_events(&dir.path().join("missing.jsonl")).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn parse_events_jsonl_handles_multiline() {
        let raw = "{\"kind\":\"retrieval\",\"ts\":\"2026-05-30T00:00:00Z\",\"device_id\":\"d\"}\n\
                   {\"kind\":\"retrieval\",\"ts\":\"2026-05-30T00:01:00Z\",\"device_id\":\"d\"}\n";
        let events = parse_events_jsonl(raw).unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn manifest_lww_prefers_remote_when_newer() {
        use crate::skill::manifest::{Content, Skill, SkillManifest, Visibility};
        use crate::skill::types::Category;
        let t1 = chrono::DateTime::from_timestamp(1_000, 0).unwrap();
        let t2 = chrono::DateTime::from_timestamp(2_000, 0).unwrap();

        let local = Skill {
            manifest: SkillManifest {
                name: "test".into(),
                version: "1.0".into(),
                publisher: "p".into(),
                description: "d".into(),
                category: Category::Context,
                scope: Default::default(),
                visibility: Visibility::default(),
                origin: None,
                origin_version: None,
                origin_hash: None,
                fleet: None,
                team: None,
                governance: None,
                project: None,
                provenance: Default::default(),
                hosts: vec![],
                content: Content {
                    r#abstract: "a".into(),
                    context: Some("c".into()),
                    procedure: None,
                    command: None,
                    note: None,
                },
                requires: vec![],
                tags: vec![],
                triggers: vec![],
                priority: Default::default(),
                evolution_log: vec![],
                transfer_chain: vec![],
                mcp_requirements: vec![],
                updated_at: t1,
                requires_programs: vec![],
            },
            content_sha256: Some("hash".into()),
            trust_level: Default::default(),
            capabilities_declared: vec![],
            publisher_signature: None,
        };

        let mut remote = local.clone();
        remote.manifest.updated_at = t2;

        let (winner, reason) = resolve_manifest_lww(local, remote, false);
        assert_eq!(reason, "remote_newer");
        assert_eq!(winner.manifest.updated_at, t2);
    }

    #[test]
    fn manifest_lww_respects_force_local() {
        use crate::skill::manifest::{Content, Skill, SkillManifest, Visibility};
        use crate::skill::types::Category;
        let t1 = chrono::DateTime::from_timestamp(1_000, 0).unwrap();
        let t2 = chrono::DateTime::from_timestamp(2_000, 0).unwrap();

        let local = Skill {
            manifest: SkillManifest {
                name: "test".into(),
                version: "1.0".into(),
                publisher: "p".into(),
                description: "d".into(),
                category: Category::Context,
                scope: Default::default(),
                visibility: Visibility::default(),
                origin: None,
                origin_version: None,
                origin_hash: None,
                fleet: None,
                team: None,
                governance: None,
                project: None,
                provenance: Default::default(),
                hosts: vec![],
                content: Content {
                    r#abstract: "a".into(),
                    context: Some("c".into()),
                    procedure: None,
                    command: None,
                    note: None,
                },
                requires: vec![],
                tags: vec![],
                triggers: vec![],
                priority: Default::default(),
                evolution_log: vec![],
                transfer_chain: vec![],
                mcp_requirements: vec![],
                updated_at: t1,
                requires_programs: vec![],
            },
            content_sha256: Some("hash".into()),
            trust_level: Default::default(),
            capabilities_declared: vec![],
            publisher_signature: None,
        };

        let mut remote = local.clone();
        remote.manifest.updated_at = t2;

        let (winner, reason) = resolve_manifest_lww(local.clone(), remote, true);
        assert_eq!(reason, "force_local");
        assert_eq!(winner.manifest.updated_at, t1);
    }
}
