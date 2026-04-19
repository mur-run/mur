//! Ingestion pipeline. Stages run normalize → dedup → filter → store.append,
//! with audit entries per successful write.
#![allow(dead_code)] // Phase 1: stubs wired in by later tasks (migrate, retrieve).

pub mod claude_code;
pub mod cursor;
pub mod dedup;
pub mod filter;
pub mod gemini;
pub mod normalize;
pub mod pipeline;
// Later tasks add: pub mod aider;

use anyhow::Result;
use mur_common::Message;

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct PullCursor {
    pub last_mtime_unix: i64,
    pub last_hash: String,
    #[serde(default)]
    pub per_file: std::collections::BTreeMap<String, FileCursor>,
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileCursor {
    pub mtime_unix: i64,
    pub last_line: u64,
}

#[derive(Debug, Clone, Copy)]
pub enum IngestStrategy {
    RealTime,
    Poll(std::time::Duration),
    Manual,
}

#[async_trait::async_trait]
pub trait Ingester: Send + Sync {
    fn name(&self) -> &'static str;
    fn strategy(&self) -> IngestStrategy;
    async fn pull(&mut self, cursor: &mut PullCursor) -> Result<Vec<Message>>;
}
