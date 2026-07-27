//! Recurrence index: which step-skeletons has harvest seen before? (#783)
//!
//! The shape gate (#781) rejects recordings by size, but size was never the real
//! question. What makes something a procedure is that it was **done more than
//! once** — a five-command deploy run once is a session; run three times it is a
//! workflow. This is the smallest thing that can tell those apart: a JSON sidecar
//! next to the inbox holding one entry per distinct skeleton, matched with the
//! same Jaccard similarity the merge suggestion already uses (a repeat is rarely
//! byte-identical).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Session ids kept per entry — enough to explain *why* a proposal exists,
/// bounded so a weekly routine cannot grow the file without limit.
const MAX_SESSIONS_PER_ENTRY: usize = 10;

/// Entries kept in total, least-recently-seen evicted first.
/// ponytail: a flat cap, not a decay policy — revisit if the index ever fills.
const MAX_ENTRIES: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Skeletonized step list — the matching key, never shown to a reviewer.
    pub skeleton: Vec<String>,
    pub count: usize,
    pub first_seen: String,
    pub last_seen: String,
    /// Sessions that produced this skeleton, oldest first, capped.
    #[serde(default)]
    pub sessions: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Index {
    #[serde(default)]
    pub entries: Vec<Entry>,
}

impl Index {
    /// Record one sighting of `skeleton`; return how many times it has now been
    /// seen. Re-observing a session already credited to an entry is a no-op, so a
    /// re-scan can never inflate a count into a false recurrence.
    pub fn observe(
        &mut self,
        skeleton: &[String],
        session_id: &str,
        now: &str,
        threshold: f32,
    ) -> usize {
        let best = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                (
                    i,
                    crate::harvest::proposal::step_similarity(skeleton, &e.skeleton),
                )
            })
            .filter(|(_, sim)| *sim >= threshold)
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i);

        match best {
            Some(i) => {
                let e = &mut self.entries[i];
                if e.sessions.iter().any(|s| s == session_id) {
                    return e.count;
                }
                e.count += 1;
                e.last_seen = now.to_string();
                e.sessions.push(session_id.to_string());
                if e.sessions.len() > MAX_SESSIONS_PER_ENTRY {
                    e.sessions.remove(0);
                }
                e.count
            }
            None => {
                self.entries.push(Entry {
                    skeleton: skeleton.to_vec(),
                    count: 1,
                    first_seen: now.to_string(),
                    last_seen: now.to_string(),
                    sessions: vec![session_id.to_string()],
                });
                self.evict();
                1
            }
        }
    }

    fn evict(&mut self) {
        if self.entries.len() <= MAX_ENTRIES {
            return;
        }
        self.entries.sort_by(|a, b| a.last_seen.cmp(&b.last_seen));
        let drop = self.entries.len() - MAX_ENTRIES;
        self.entries.drain(..drop);
    }
}

/// Sidecar path. Dotfile so the inbox's `*.yaml` proposal listing never sees it.
pub fn path_for(inbox_dir: &Path) -> PathBuf {
    inbox_dir.join(".skeleton-index.json")
}

/// Load the index; a missing or corrupt file starts a fresh one rather than
/// failing the scan — the index is derived state, never a source of truth.
pub fn load(path: &Path) -> Index {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_default()
}

pub fn save(path: &Path, index: &Index) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(index)?)?;
    std::fs::rename(&tmp, path).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steps(cmds: &[&str]) -> Vec<String> {
        cmds.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn first_sighting_counts_once() {
        let mut idx = Index::default();
        let n = idx.observe(&steps(&["cargo build", "cargo test"]), "s1", "t0", 0.6);
        assert_eq!(n, 1);
        assert_eq!(idx.entries.len(), 1);
    }

    #[test]
    fn near_identical_repeat_increments_the_same_entry() {
        let mut idx = Index::default();
        idx.observe(&steps(&["cargo build", "cargo test"]), "s1", "t0", 0.6);
        let n = idx.observe(
            &steps(&["cargo build", "cargo test", "cargo test"]),
            "s2",
            "t1",
            0.6,
        );
        assert_eq!(n, 2);
        assert_eq!(idx.entries.len(), 1, "should not have forked an entry");
        assert_eq!(idx.entries[0].sessions, vec!["s1", "s2"]);
    }

    #[test]
    fn unrelated_session_starts_its_own_entry() {
        let mut idx = Index::default();
        idx.observe(&steps(&["cargo build", "cargo test"]), "s1", "t0", 0.6);
        let n = idx.observe(&steps(&["npm run lint"]), "s2", "t1", 0.6);
        assert_eq!(n, 1);
        assert_eq!(idx.entries.len(), 2);
    }

    #[test]
    fn re_observing_the_same_session_does_not_inflate_the_count() {
        let mut idx = Index::default();
        let s = steps(&["cargo build", "cargo test"]);
        idx.observe(&s, "s1", "t0", 0.6);
        assert_eq!(idx.observe(&s, "s1", "t1", 0.6), 1);
    }

    #[test]
    fn round_trips_through_disk() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = path_for(&tmp.path().join("inbox"));
        let mut idx = Index::default();
        idx.observe(&steps(&["cargo build"]), "s1", "t0", 0.6);
        save(&path, &idx).unwrap();
        assert_eq!(load(&path).entries.len(), 1);
    }

    #[test]
    fn missing_file_loads_empty() {
        assert!(
            load(Path::new("/nonexistent/.skeleton-index.json"))
                .entries
                .is_empty()
        );
    }
}
