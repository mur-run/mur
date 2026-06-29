#![allow(dead_code, unused_imports)]
//! LMDB-backed score cache via the `heed` crate.
//!
//! Scores are keyed by content hash (not track ID or session), so identical
//! implementations across sessions are only judged once.

use std::path::Path;

use anyhow::Result;
use heed::types::{Bytes, SerdeJson};
use heed::{Database, Env, EnvOpenOptions};

use super::JudgeScore;

pub struct ParallelStateDb {
    env: Env,
    scores: Database<Bytes, SerdeJson<JudgeScore>>,
}

impl ParallelStateDb {
    /// Open (or create) the LMDB environment at `dir`.
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        // SAFETY: standard heed usage; no other process opens this env concurrently.
        let env = unsafe {
            EnvOpenOptions::new()
                .map_size(50 * 1024 * 1024) // 50 MB
                .max_dbs(3)
                .open(dir)?
        };
        let mut wtxn = env.write_txn()?;
        let scores = env.create_database(&mut wtxn, Some("scores"))?;
        wtxn.commit()?;
        Ok(Self { env, scores })
    }

    /// Build the lookup key: `"{hex_of_hash}:{rubric_version}"` as UTF-8 bytes.
    fn score_key(content_hash: &[u8; 32], rubric_version: &str) -> Vec<u8> {
        let mut k = hex::encode(content_hash).into_bytes();
        k.push(b':');
        k.extend_from_slice(rubric_version.as_bytes());
        k
    }

    /// Retrieve a previously stored score, or `None` if not yet judged.
    pub fn get_score(
        &self,
        content_hash: &[u8; 32],
        rubric_version: &str,
    ) -> Result<Option<JudgeScore>> {
        let rtxn = self.env.read_txn()?;
        let key = Self::score_key(content_hash, rubric_version);
        Ok(self.scores.get(&rtxn, &key)?)
    }

    /// Persist a judge score for the given content hash + rubric version.
    pub fn put_score(
        &self,
        content_hash: &[u8; 32],
        rubric_version: &str,
        score: &JudgeScore,
    ) -> Result<()> {
        let mut wtxn = self.env.write_txn()?;
        let key = Self::score_key(content_hash, rubric_version);
        self.scores.put(&mut wtxn, &key, score)?;
        wtxn.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parallel::state::JudgeScore;

    #[test]
    fn score_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let db = ParallelStateDb::open(dir.path()).unwrap();
        let hash = [7u8; 32];
        let score = JudgeScore {
            score: 8.5,
            reasoning: "good design".into(),
            model: "claude-opus-4-8".into(),
            ts: 1_700_000_000,
        };
        db.put_score(&hash, "v1", &score).unwrap();
        let retrieved = db.get_score(&hash, "v1").unwrap().unwrap();
        assert!((retrieved.score - 8.5).abs() < 0.001);
        assert_eq!(retrieved.reasoning, "good design");
    }

    #[test]
    fn missing_score_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let db = ParallelStateDb::open(dir.path()).unwrap();
        assert!(db.get_score(&[0u8; 32], "v1").unwrap().is_none());
    }
}
