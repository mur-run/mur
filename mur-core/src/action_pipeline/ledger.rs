use mur_common::action::ActionEvent;
use mur_common::ledger::Ledger as GenericLedger;
use std::path::Path;

/// Shared action-pipeline ledger wrapping the generic `Ledger<ActionEvent>`.
pub struct ActionLedger {
    inner: GenericLedger<ActionEvent>,
}

impl ActionLedger {
    pub fn open(base_dir: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            inner: GenericLedger::open(base_dir)?,
        })
    }

    pub fn append(&mut self, event: &ActionEvent) -> anyhow::Result<()> {
        self.inner.append(event)
    }

    pub fn flush(&mut self) -> anyhow::Result<()> {
        self.inner.flush()
    }

    /// Replay today's ledger events to rebuild in-memory state.
    pub fn replay_today(base_dir: &Path) -> Vec<ActionEvent> {
        GenericLedger::<ActionEvent>::scan_days(base_dir, 1)
            .into_iter()
            .filter_map(|r| r.ok())
            .collect()
    }

    /// Replay the last `days` of ledger events.
    pub fn replay_days(base_dir: &Path, days: u32) -> Vec<ActionEvent> {
        GenericLedger::<ActionEvent>::scan_days(base_dir, days)
            .into_iter()
            .filter_map(|r| r.ok())
            .collect()
    }
}
