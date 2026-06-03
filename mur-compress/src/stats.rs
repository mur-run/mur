//! Persistent savings stats (atomic JSON at <store>/stats.json).

use std::path::PathBuf;
use std::sync::Mutex;

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

    pub fn record_compression(&self, before: usize, after: usize) {
        let mut d = self.inner.lock().unwrap();
        d.compressions += 1;
        d.total_input_tokens += before as u64;
        d.total_output_tokens += after as u64;
        d.total_tokens_saved += before.saturating_sub(after) as u64;
        self.flush(&d);
    }

    pub fn record_retrieval(&self) {
        let mut d = self.inner.lock().unwrap();
        d.retrievals += 1;
        self.flush(&d);
    }

    pub fn snapshot(
        &self,
        cost_per_mtok_usd: f64,
        store_entries: usize,
        store_bytes: u64,
    ) -> StatsSnapshot {
        let d = self.inner.lock().unwrap().clone();
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
}
