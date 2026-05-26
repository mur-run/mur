//! Per-skill runtime statistics. **Not** signed, **not** part of the
//! publisher manifest. Lives at `<MUR_HOME>/skills/<name>/stats.json`
//! and is rebuildable from the JSONL trace log via
//! `mur skill reindex-stats`.
//!
//! ## Security
//!
//! `stats.json` is host-local mutable state and is **explicitly outside
//! the DSSE signature scope** (see §2.2 Layer 1 of the skill ecosystem
//! design). A skill's signature covers `skill.yaml` only. Stats can be
//! deleted or rebuilt (`mur skill reindex-stats`) without affecting
//! trust.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fd_lock::RwLock;
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

pub const STATS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Copy, Default)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    #[default]
    Draft,
    Emerging,
    Stable,
    Canonical,
    Deprecated,
    Archived,
}

/// Sidecar stats for an installed skill. **Not part of the signed manifest.**
///
/// Schema evolution policy: additive only. New fields MUST be marked
/// `#[serde(default)]` so older `mur` builds reading newer files (and newer
/// builds reading older files) parse cleanly without migration. Do not
/// pre-reserve fields without a producer — empty defaults create semantic
/// ambiguity ("never set" vs "set to empty"). Add fields when their
/// callers exist.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillStats {
    pub schema_version: u32,
    pub skill_name: String,
    pub skill_version: String,
    /// SHA-256 of the manifest content at the time these stats were
    /// (re)initialised. A mismatch on load tells us the skill was
    /// reinstalled — see `reset_on_manifest_change()`.
    pub manifest_digest: String,

    pub lifecycle_state: LifecycleState,
    pub lifecycle_changed_at: DateTime<Utc>,
    pub pinned: bool,
    #[serde(default)]
    pub pinned_reason: String,

    pub usage_count: u64,
    pub success_count: u64,
    pub failure_count: u64,

    pub last_used_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub first_successful_use_at: Option<DateTime<Utc>>,

    /// Confidence at the moment of the most recent successful use (or
    /// most recent promotion — see `lifecycle::on_promotion`). Decay is
    /// computed *from this anchor*, never incrementally — keeps the
    /// value numerically stable and idempotent on read.
    pub anchor_confidence: f64,

    /// Watermark for incremental reindex — the trace timestamp that
    /// these stats have already absorbed. `mur skill reindex-stats`
    /// resumes from here.
    pub rebuilt_from_trace_through: Option<DateTime<Utc>>,

    /// Count of inject-time `Resolution::Unresolved` outcomes for this skill.
    /// A spike here means the skill declares intents that no longer match the
    /// agent's MCP inventory — doctor's `intent-resolvable` check surfaces this.
    #[serde(default)]
    pub resolution_misses: u64,
}

impl SkillStats {
    pub fn new(
        skill_name: &str,
        skill_version: &str,
        manifest_digest: &str,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            schema_version: STATS_SCHEMA_VERSION,
            skill_name: skill_name.to_string(),
            skill_version: skill_version.to_string(),
            manifest_digest: manifest_digest.to_string(),
            lifecycle_state: LifecycleState::default(),
            lifecycle_changed_at: now,
            pinned: false,
            pinned_reason: String::new(),
            usage_count: 0,
            success_count: 0,
            failure_count: 0,
            last_used_at: None,
            last_success_at: None,
            first_successful_use_at: None,
            anchor_confidence: 1.0,
            rebuilt_from_trace_through: None,
            resolution_misses: 0,
        }
    }

    pub fn path(mur_home: &Path, skill_name: &str) -> PathBuf {
        mur_home.join("skills").join(skill_name).join("stats.json")
    }

    /// Read the sidecar, or return `None` if absent. Lock-free — fine
    /// for read-mostly callers (doctor, info, stats). Concurrent writers
    /// going through `merge_in_place` will not corrupt the file because
    /// they hold the exclusive lock during the write window.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        match std::fs::read_to_string(path) {
            Ok(s) => {
                let stats: Self = serde_json::from_str(&s).context("deserialise stats.json")?;
                Ok(Some(stats))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).context("read stats.json"),
        }
    }

    /// Read-merge-write under an exclusive `fd-lock`. `merge_fn` is
    /// called with the loaded value (or the supplied default if none
    /// exists) and is responsible for applying the delta. The lock
    /// window is microseconds — counter increments only.
    pub fn merge_in_place(
        path: &Path,
        default: impl FnOnce() -> Self,
        merge_fn: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<()> {
        // Lock on a sidecar lockfile, not stats.json itself — POSIX
        // flock(2) on the data file would race with rename. Same
        // pattern as git/index.lock.
        let lock_path = path.with_extension("lock");
        let parent = path.parent().context("stats path has no parent")?;
        std::fs::create_dir_all(parent).ok();

        let mut lock_file = RwLock::new(
            OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .read(true)
                .open(&lock_path)
                .context("open stats lockfile")?,
        );
        let _guard = lock_file.write().context("acquire stats lock")?;

        let mut stats = Self::load(path)?.unwrap_or_else(default);
        merge_fn(&mut stats)?;

        let tmp = NamedTempFile::new_in(parent).context("create temp file for stats")?;
        serde_json::to_writer_pretty(&tmp, &stats).context("serialise stats")?;
        tmp.persist(path).context("persist stats")?;
        Ok(())
    }

    /// Returns true if the loaded stats refer to a different manifest
    /// digest than the one currently installed. Callers (the aggregator
    /// and reindex) should `reset()` in that case rather than carry
    /// counters across an upgrade.
    ///
    /// A version bump resets `usage_count` / `success_count` /
    /// `failure_count` but **preserves** `pinned`,
    /// `first_successful_use_at`, and `lifecycle_state` (a Canonical
    /// skill bumping to 1.2.0 should not regress to Draft).
    pub fn is_stale(&self, current_digest: &str) -> bool {
        self.manifest_digest != current_digest
    }

    /// Reset counters for a manifest change, preserving pinned state
    /// and first-success timestamp.
    pub fn reset_for_new_manifest(
        &mut self,
        new_version: &str,
        new_digest: &str,
        now: DateTime<Utc>,
    ) {
        self.skill_version = new_version.to_string();
        self.manifest_digest = new_digest.to_string();
        self.usage_count = 0;
        self.success_count = 0;
        self.failure_count = 0;
        self.last_used_at = None;
        self.last_success_at = None;
        self.anchor_confidence = 1.0;
        self.rebuilt_from_trace_through = None;
        self.lifecycle_changed_at = now;
        // Preserve: pinned, pinned_reason, first_successful_use_at, lifecycle_state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn temp_stats_path() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_skill").join("stats.json");
        let parent = path.parent().unwrap();
        std::fs::create_dir_all(parent).unwrap();
        (dir, path)
    }

    fn dummy_stats(name: &str) -> SkillStats {
        SkillStats::new(name, "1.0.0", "abc123", Utc::now())
    }

    #[test]
    fn load_returns_none_for_missing_path() {
        let (_dir, path) = temp_stats_path();
        let result = SkillStats::load(&path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn load_returns_stats_for_valid_file() {
        let (_dir, path) = temp_stats_path();
        let stats = dummy_stats("test-skill");
        std::fs::write(&path, serde_json::to_string_pretty(&stats).unwrap()).unwrap();
        let loaded = SkillStats::load(&path).unwrap().unwrap();
        assert_eq!(loaded.skill_name, "test-skill");
        assert_eq!(loaded.usage_count, 0);
    }

    #[test]
    fn merge_in_place_counter_increment() {
        let (_dir, path) = temp_stats_path();
        let skill_name = "merge-test".to_string();
        let default = || dummy_stats(&skill_name);

        // First merge: increment usage
        SkillStats::merge_in_place(&path, default, |s| {
            s.usage_count += 1;
            Ok(())
        })
        .unwrap();

        let loaded = SkillStats::load(&path).unwrap().unwrap();
        assert_eq!(loaded.usage_count, 1);

        // Second merge: increment again
        SkillStats::merge_in_place(
            &path,
            || panic!("default should not be called"),
            |s| {
                s.usage_count += 2;
                Ok(())
            },
        )
        .unwrap();

        let loaded = SkillStats::load(&path).unwrap().unwrap();
        assert_eq!(loaded.usage_count, 3);
    }

    #[test]
    fn concurrent_merge_both_increments_commit() {
        let (_dir, path) = temp_stats_path();
        let skill_name = "concurrent-test".to_string();
        let path = std::path::PathBuf::from(path); // decouple from tempdir lifetime
        let path2 = path.clone();

        // Init the file
        SkillStats::merge_in_place(&path, || dummy_stats(&skill_name), |_| Ok(())).unwrap();

        let t1 = thread::spawn(move || {
            SkillStats::merge_in_place(
                &path,
                || panic!("default should not be called"),
                |s| {
                    s.usage_count += 1;
                    Ok(())
                },
            )
            .unwrap();
        });
        let t2 = thread::spawn(move || {
            SkillStats::merge_in_place(
                &path2,
                || panic!("default should not be called"),
                |s| {
                    s.usage_count += 2;
                    Ok(())
                },
            )
            .unwrap();
        });

        t1.join().unwrap();
        t2.join().unwrap();

        let loaded = SkillStats::load(&_dir.path().join("test_skill").join("stats.json"))
            .unwrap()
            .unwrap();
        // Both increments should have committed (commutative counters)
        assert_eq!(loaded.usage_count, 3);
    }

    #[test]
    fn is_stale_detects_digest_mismatch() {
        let stats = dummy_stats("test");
        assert!(!stats.is_stale("abc123"));
        assert!(stats.is_stale("different"));
    }

    #[test]
    fn schema_version_1_deserialises_fixture() {
        let fixture = r#"{
            "schema_version": 1,
            "skill_name": "research-patterns",
            "skill_version": "2.3.0",
            "manifest_digest": "abcdef",
            "lifecycle_state": "emerging",
            "lifecycle_changed_at": "2026-05-25T00:00:00Z",
            "pinned": false,
            "pinned_reason": "",
            "usage_count": 42,
            "success_count": 38,
            "failure_count": 4,
            "last_used_at": "2026-05-25T12:00:00Z",
            "last_success_at": "2026-05-25T11:00:00Z",
            "first_successful_use_at": "2026-05-01T00:00:00Z",
            "anchor_confidence": 0.95,
            "rebuilt_from_trace_through": "2026-05-25T10:00:00Z"
        }"#;
        let stats: SkillStats = serde_json::from_str(fixture).unwrap();
        assert_eq!(stats.schema_version, 1);
        assert_eq!(stats.lifecycle_state, LifecycleState::Emerging);
        assert_eq!(stats.usage_count, 42);
        assert_eq!(stats.anchor_confidence, 0.95);
        assert!(stats.last_used_at.is_some());
    }

    #[test]
    fn reset_for_new_manifest_preserves_pinned_and_state() {
        let mut stats = SkillStats {
            pinned: true,
            pinned_reason: "critical".into(),
            lifecycle_state: LifecycleState::Canonical,
            first_successful_use_at: Some(Utc::now()),
            usage_count: 100,
            success_count: 95,
            failure_count: 5,
            ..dummy_stats("test")
        };
        stats.reset_for_new_manifest("2.0.0", "newdigest", Utc::now());
        assert_eq!(stats.skill_version, "2.0.0");
        assert_eq!(stats.usage_count, 0);
        assert!(stats.pinned);
        assert_eq!(stats.lifecycle_state, LifecycleState::Canonical);
        assert!(stats.first_successful_use_at.is_some());
    }
}
