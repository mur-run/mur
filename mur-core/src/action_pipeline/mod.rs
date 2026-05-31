pub mod error;
pub mod guard;
pub mod ingest;
pub mod ledger;
pub mod notify;
pub mod queue;

pub use error::PipelineError;
pub use ingest::PendingStore;
pub use ledger::ActionLedger;

use mur_common::action::ActionPipelineConfig;
use std::path::PathBuf;

/// Top-level entry point for the action pipeline.
#[derive(Clone)]
pub struct Pipeline {
    pub agent_home: PathBuf,
    pub config: ActionPipelineConfig,
}

impl Pipeline {
    pub fn new(agent_home: PathBuf, config: ActionPipelineConfig) -> Self {
        Self { agent_home, config }
    }

    /// Directory layout:
    ///   <agent_home>/actions/ledger/   — daily JSONL files
    ///   <agent_home>/actions/pending.json — rebuildable snapshot
    ///   <agent_home>/trash/            — trashed files
    pub fn actions_dir(&self) -> PathBuf {
        self.agent_home.join("actions")
    }

    pub fn ledger_dir(&self) -> PathBuf {
        self.actions_dir().join("ledger")
    }

    pub fn pending_snapshot_path(&self) -> PathBuf {
        self.actions_dir().join("pending.json")
    }

    pub fn trash_dir(&self) -> PathBuf {
        self.agent_home.join("trash")
    }
}
