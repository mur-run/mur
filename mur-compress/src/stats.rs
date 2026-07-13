//! Persistent savings stats (atomic JSON at <store>/stats.json).
//!
//! Updates are safe across processes/engines: every mutation takes an advisory
//! lock on a sidecar `stats.lock`, re-reads the on-disk totals, applies its
//! delta, and writes back. Without this, two engines (the MCP server builds a
//! fresh one per call) would each read the same baseline and clobber each
//! other's increment, undercounting savings.
//!
//! Two views are kept side by side:
//! - The flat `StatsData` fields are **all-time totals**: every compression
//!   and retrieval ever recorded, with no version or date attribution. They
//!   keep accumulating exactly as they always have.
//! - `StatsData::buckets` **additionally** segments the same events by
//!   `(mur_version, local day)`, so releases can be compared against each
//!   other. It is populated only from the moment this field shipped onward;
//!   there is no raw event log to retroactively bucket older history into.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::Local;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

/// All-time running totals, exactly as before segmentation was added: every
/// compression/retrieval increments these regardless of version or date, and
/// they are never reset or migrated. This is the only place history from
/// before per-version/per-day buckets existed lives -- `buckets` is purely
/// additive and only ever populated going forward.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StatsData {
    compressions: u64,
    retrievals: u64,
    total_input_tokens: u64,
    total_output_tokens: u64,
    total_tokens_saved: u64,
    #[serde(default)]
    skipped: u64,

    /// Per-version, per-day segmentation: `mur_version -> "YYYY-MM-DD"
    /// (local date) -> counts for that version on that day`. Absent for any
    /// activity recorded before this field existed (old `stats.json` files
    /// deserialize with this empty via `#[serde(default)]`); that older
    /// history is only reflected in the flat totals above, not retroactively
    /// bucketed.
    #[serde(default)]
    buckets: HashMap<String, HashMap<String, BucketData>>,

    /// Per-`ContentType::as_str()` totals (code/json/search_results/build_log/
    /// git_diff/generic), so we can see where tokens actually sit and which
    /// content types compress well vs. barely at all. Additive only, same
    /// backfill rule as `buckets`.
    #[serde(default)]
    by_type: HashMap<String, TypeStats>,
}

/// Running totals for one content type across all-time compressions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TypeStats {
    pub compressions: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_tokens_saved: u64,
    #[serde(default)]
    pub skipped: u64,
}

/// Counts for one `(mur_version, day)` bucket. Mirrors the shape of the
/// all-time totals, minus the derived fields (`savings_percent` etc.), which
/// are computed from these at read time so they can never drift out of sync.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BucketData {
    pub compressions: u64,
    pub retrievals: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_tokens_saved: u64,
    #[serde(default)]
    pub skipped: u64,
}

#[derive(Debug, Clone)]
pub struct StatsSnapshot {
    pub compressions: u64,
    pub retrievals: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_tokens_saved: u64,
    pub skipped: u64,
    pub savings_percent: f32,
    pub estimated_cost_saved_usd: f64,
    pub store_entries: usize,
    pub store_bytes: u64,

    /// Raw per-`(mur_version, day)` segmentation, straight from
    /// `StatsData::buckets`. Formatting/rollup (recent-N days, per-version
    /// sums, etc.) is left to consumers (MCP tool, CLI) rather than computed
    /// here, so this stays a thin, order-independent map.
    pub buckets: HashMap<String, HashMap<String, BucketData>>,

    /// Raw per-content-type totals, straight from `StatsData::by_type`.
    pub by_type: HashMap<String, TypeStats>,
}

/// Current crate version, used as the bucket's version key. `mur-compress`
/// inherits the workspace version, so this is stable across every surface
/// that links it (MCP server, CLI, PostToolUse hooks) without needing the
/// version threaded in from each call site.
fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Today's bucket key in local time, `"YYYY-MM-DD"`. Local (not UTC) so a
/// day boundary matches a release's users' wall-clock day.
fn today_key() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

pub struct StatsTracker {
    path: PathBuf,
    inner: Mutex<StatsData>,
}

impl StatsTracker {
    pub fn new(path: PathBuf) -> Self {
        let inner = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<StatsData>(&b).ok())
            .unwrap_or_default();
        Self {
            path,
            inner: Mutex::new(inner),
        }
    }

    fn flush(&self, d: &StatsData) {
        if let Ok(bytes) = serde_json::to_vec(d) {
            let tmp = self.path.with_extension("tmp");
            if std::fs::write(&tmp, &bytes).is_ok() {
                let _ = std::fs::rename(&tmp, &self.path);
            }
        }
    }

    fn read_disk(&self) -> Option<StatsData> {
        std::fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice::<StatsData>(&b).ok())
    }

    /// Apply `delta` to the freshest on-disk totals under an advisory lock, so
    /// concurrent engines accumulate instead of clobbering each other.
    fn update(&self, delta: impl FnOnce(&mut StatsData)) {
        let mut guard = self.inner.lock().unwrap();
        // Sidecar lock file: the data file is replaced via rename (new inode),
        // so it can't itself be a stable lock target.
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.path.with_extension("lock"))
            .ok();
        if let Some(l) = &lock {
            let _ = l.lock_exclusive();
        }
        let mut data = self.read_disk().unwrap_or_else(|| guard.clone());
        delta(&mut data);
        self.flush(&data);
        *guard = data;
        if let Some(l) = &lock {
            let _ = FileExt::unlock(l);
        }
    }

    pub fn record_compression(&self, content_type: &str, before: usize, after: usize) {
        let day = today_key();
        let saved = before.saturating_sub(after) as u64;
        self.update(|d| {
            d.compressions += 1;
            d.total_input_tokens += before as u64;
            d.total_output_tokens += after as u64;
            d.total_tokens_saved += saved;

            let bucket = d
                .buckets
                .entry(current_version().to_string())
                .or_default()
                .entry(day.clone())
                .or_default();
            bucket.compressions += 1;
            bucket.total_input_tokens += before as u64;
            bucket.total_output_tokens += after as u64;
            bucket.total_tokens_saved += saved;

            let ts = d.by_type.entry(content_type.to_string()).or_default();
            ts.compressions += 1;
            ts.total_input_tokens += before as u64;
            ts.total_output_tokens += after as u64;
            ts.total_tokens_saved += saved;
        });
    }

    pub fn record_retrieval(&self) {
        let day = today_key();
        self.update(|d| {
            d.retrievals += 1;
            d.buckets
                .entry(current_version().to_string())
                .or_default()
                .entry(day)
                .or_default()
                .retrievals += 1;
        });
    }

    /// Record a deliberate skip (early-skip gate): the input was inspected
    /// and passed through untouched. Counted separately from compressions
    /// so by_type ratios reflect real compression work.
    pub fn record_skip(&self, content_type: &str) {
        let day = today_key();
        self.update(|d| {
            d.skipped += 1;
            d.buckets
                .entry(current_version().to_string())
                .or_default()
                .entry(day)
                .or_default()
                .skipped += 1;
            d.by_type
                .entry(content_type.to_string())
                .or_default()
                .skipped += 1;
        });
    }

    pub fn snapshot(
        &self,
        cost_per_mtok_usd: f64,
        store_entries: usize,
        store_bytes: u64,
    ) -> StatsSnapshot {
        // Prefer the on-disk totals so a snapshot reflects writes from other
        // engines/processes, not just this instance's view.
        let d = self
            .read_disk()
            .unwrap_or_else(|| self.inner.lock().unwrap().clone());
        let pct = if d.total_input_tokens > 0 {
            d.total_tokens_saved as f32 / d.total_input_tokens as f32 * 100.0
        } else {
            0.0
        };
        let cost = d.total_tokens_saved as f64 * cost_per_mtok_usd / 1_000_000.0;
        StatsSnapshot {
            compressions: d.compressions,
            retrievals: d.retrievals,
            total_input_tokens: d.total_input_tokens,
            total_output_tokens: d.total_output_tokens,
            total_tokens_saved: d.total_tokens_saved,
            skipped: d.skipped,
            savings_percent: pct,
            estimated_cost_saved_usd: cost,
            store_entries,
            store_bytes,
            buckets: d.buckets,
            by_type: d.by_type,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulates_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.json");
        let t = StatsTracker::new(path.clone());
        t.record_compression("generic", 100, 30);
        t.record_retrieval();
        let snap = t.snapshot(3.0, 1, 123);
        assert_eq!(snap.compressions, 1);
        assert_eq!(snap.total_tokens_saved, 70);
        assert!((snap.savings_percent - 70.0).abs() < 0.01);
        // reload from disk
        let t2 = StatsTracker::new(path);
        let snap2 = t2.snapshot(3.0, 0, 0);
        assert_eq!(snap2.total_tokens_saved, 70);
    }

    #[test]
    fn concurrent_instances_accumulate_without_clobbering() {
        // Two trackers on the same file (as the MCP server's per-call engines
        // would be) must sum their deltas, not overwrite each other.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.json");
        let a = StatsTracker::new(path.clone());
        let b = StatsTracker::new(path.clone());
        a.record_compression("generic", 100, 30); // saved 70
        b.record_compression("generic", 50, 10); // saved 40
        a.record_retrieval();
        b.record_retrieval();

        let snap = StatsTracker::new(path).snapshot(3.0, 0, 0);
        assert_eq!(snap.compressions, 2);
        assert_eq!(snap.retrievals, 2);
        assert_eq!(snap.total_input_tokens, 150);
        assert_eq!(snap.total_tokens_saved, 110);
    }

    #[test]
    fn record_compression_updates_bucket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.json");
        let t = StatsTracker::new(path.clone());
        t.record_compression("generic", 100, 30); // saved 70
        t.record_compression("generic", 50, 10); // saved 40

        let snap = t.snapshot(3.0, 0, 0);
        assert_eq!(snap.compressions, 2);
        assert_eq!(snap.total_input_tokens, 150);
        assert_eq!(snap.total_tokens_saved, 110);

        let day = today_key();
        let bucket = snap
            .buckets
            .get(current_version())
            .and_then(|days| days.get(&day))
            .expect("bucket for current version/day should exist");
        assert_eq!(bucket.compressions, 2);
        assert_eq!(bucket.total_input_tokens, 150);
        assert_eq!(bucket.total_output_tokens, 40);
        assert_eq!(bucket.total_tokens_saved, 110);
    }

    #[test]
    fn old_stats_json_without_buckets_deserializes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.json");
        // Legacy on-disk shape: only the flat fields, no `buckets` key at all.
        std::fs::write(
            &path,
            r#"{"compressions":7,"retrievals":3,"total_input_tokens":2150000,"total_output_tokens":120000,"total_tokens_saved":2030000}"#,
        )
        .unwrap();

        let t = StatsTracker::new(path);
        let snap = t.snapshot(3.0, 0, 0);
        assert_eq!(snap.compressions, 7);
        assert_eq!(snap.retrievals, 3);
        assert_eq!(snap.total_input_tokens, 2150000);
        assert_eq!(snap.total_output_tokens, 120000);
        assert_eq!(snap.total_tokens_saved, 2030000);
        assert!(snap.buckets.is_empty());
    }

    #[test]
    fn snapshot_exposes_buckets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.json");
        let t = StatsTracker::new(path);
        t.record_compression("generic", 200, 20); // saved 180
        t.record_retrieval();

        let snap = t.snapshot(3.0, 0, 0);
        let day = today_key();
        let bucket = snap
            .buckets
            .get(current_version())
            .and_then(|days| days.get(&day))
            .expect("bucket for current version/day should exist");
        assert_eq!(bucket.compressions, 1);
        assert_eq!(bucket.retrievals, 1);
        assert_eq!(bucket.total_tokens_saved, 180);
    }

    #[test]
    fn snapshot_exposes_by_type_breakdown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stats.json");
        let t = StatsTracker::new(path);
        t.record_compression("code", 1000, 900); // saved 100, barely compresses
        t.record_compression("json", 1000, 100); // saved 900, compresses well
        t.record_compression("json", 500, 50); // saved 450

        let snap = t.snapshot(3.0, 0, 0);
        let code = snap.by_type.get("code").expect("code entry");
        assert_eq!(code.compressions, 1);
        assert_eq!(code.total_tokens_saved, 100);

        let json = snap.by_type.get("json").expect("json entry");
        assert_eq!(json.compressions, 2);
        assert_eq!(json.total_input_tokens, 1500);
        assert_eq!(json.total_tokens_saved, 1350);
    }

    #[test]
    fn record_skip_counts_separately() {
        let dir = tempfile::tempdir().unwrap();
        let t = StatsTracker::new(dir.path().join("stats.json"));
        t.record_compression("generic", 100, 50);
        t.record_skip("generic");
        t.record_skip("generic");
        let snap = t.snapshot(3.0, 0, 0);
        assert_eq!(snap.compressions, 1);
        assert_eq!(snap.skipped, 2);
        assert_eq!(snap.by_type.get("generic").unwrap().skipped, 2);
        // compressions untouched by skips
        assert_eq!(snap.by_type.get("generic").unwrap().compressions, 1);
    }

    #[test]
    fn old_stats_json_without_skipped_loads() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("stats.json");
        std::fs::write(&p, r#"{"compressions":5,"retrievals":1,"total_input_tokens":10,"total_output_tokens":5,"total_tokens_saved":5}"#).unwrap();
        let t = StatsTracker::new(p);
        let snap = t.snapshot(3.0, 0, 0);
        assert_eq!(snap.compressions, 5);
        assert_eq!(snap.skipped, 0);
    }
}
