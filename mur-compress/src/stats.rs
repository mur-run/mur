//! Persistent savings stats (atomic JSON at <store>/stats.json).
//!
//! Updates are safe across processes/engines: every mutation takes an advisory
//! lock on a sidecar `stats.lock`, re-reads the on-disk totals, applies its
//! delta, and writes back. Without this, two engines (the MCP server builds a
//! fresh one per call) would each read the same baseline and clobber each
//! other's increment, undercounting savings.

use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::Mutex;

use fs2::FileExt;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StatsData {
    compressions: u64,
    retrievals: u64,
    total_input_tokens: u64,
    total_output_tokens: u64,
    total_tokens_saved: u64,
}

#[derive(Debug, Clone)]
pub struct StatsSnapshot {
    pub compressions: u64,
    pub retrievals: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_tokens_saved: u64,
    pub savings_percent: f32,
    pub estimated_cost_saved_usd: f64,
    pub store_entries: usize,
    pub store_bytes: u64,
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

    pub fn record_compression(&self, before: usize, after: usize) {
        self.update(|d| {
            d.compressions += 1;
            d.total_input_tokens += before as u64;
            d.total_output_tokens += after as u64;
            d.total_tokens_saved += before.saturating_sub(after) as u64;
        });
    }

    pub fn record_retrieval(&self) {
        self.update(|d| d.retrievals += 1);
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
            savings_percent: pct,
            estimated_cost_saved_usd: cost,
            store_entries,
            store_bytes,
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
        t.record_compression(100, 30);
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
        a.record_compression(100, 30); // saved 70
        b.record_compression(50, 10); // saved 40
        a.record_retrieval();
        b.record_retrieval();

        let snap = StatsTracker::new(path).snapshot(3.0, 0, 0);
        assert_eq!(snap.compressions, 2);
        assert_eq!(snap.retrievals, 2);
        assert_eq!(snap.total_input_tokens, 150);
        assert_eq!(snap.total_tokens_saved, 110);
    }
}
