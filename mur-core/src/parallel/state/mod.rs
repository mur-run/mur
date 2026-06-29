//! Parallel run state management — persistent score cache.

pub mod lmdb;
pub use lmdb::ParallelStateDb;

use serde::{Deserialize, Serialize};

/// A judge's score for one implementation, keyed by content hash (not track/session).
/// Identical implementations across sessions are only judged once.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeScore {
    pub score: f32,
    pub reasoning: String,
    pub model: String,
    pub ts: u64,
}
