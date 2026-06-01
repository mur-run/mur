//! Escalation audit ledger for the cost-router orchestrator.
//!
//! Wraps `mur_common::ledger::Ledger<EscalationEvent>` with a
//! domain-specific API. Stored at `~/.mur/route/ledger/YYYY-MM-DD.jsonl`.
//!
//! This is the cost-visibility surface: every escalation decision is
//! recorded so the savings thesis is measurable before Phase 2 (spawn).

use mur_common::ledger::Ledger as GenericLedger;
use mur_common::route::EscalationEvent;
use std::path::Path;

/// Ledger for escalation routing decisions.
///
/// One JSONL record per routing decision that either escalated to frontier
/// or would-have-escalated. The ledger lives at `~/.mur/route/ledger/`.
pub struct EscalationLedger {
    inner: GenericLedger<EscalationEvent>,
}

impl EscalationLedger {
    /// Open (or create) the escalation ledger directory.
    /// Default path: `~/.mur/route/ledger/`.
    pub fn open(base_dir: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            inner: GenericLedger::open(base_dir)?,
        })
    }

    /// Open the ledger at the default path (`~/.mur/route/ledger/`).
    pub fn open_default() -> anyhow::Result<Self> {
        // Reuse the canonical root resolver instead of re-implementing it.
        let path = crate::paths::mur_root(None).join("route").join("ledger");
        Self::open(&path)
    }

    /// Append one escalation event to today's JSONL file.
    pub fn append(&mut self, event: &EscalationEvent) -> anyhow::Result<()> {
        self.inner.append(event)
    }

    /// Flush pending writes to disk.
    pub fn flush(&mut self) -> anyhow::Result<()> {
        self.inner.flush()
    }

    /// Replay today's ledger events.
    pub fn replay_today(base_dir: &Path) -> Vec<EscalationEvent> {
        GenericLedger::<EscalationEvent>::scan_days(base_dir, 1)
            .into_iter()
            .filter_map(|r| r.ok())
            .collect()
    }

    /// Replay the last `days` of ledger events.
    pub fn replay_days(base_dir: &Path, days: u32) -> Vec<EscalationEvent> {
        GenericLedger::<EscalationEvent>::scan_days(base_dir, days)
            .into_iter()
            .filter_map(|r| r.ok())
            .collect()
    }

    /// Count escalation rate over the last `days`.
    /// Returns (escalations, total_decisions, rate).
    pub fn escalation_rate(base_dir: &Path, days: u32) -> (usize, usize, f64) {
        let s = Self::summary(base_dir, days);
        (s.escalations, s.total, s.rate)
    }

    /// Aggregate escalation **and cost** KPIs over the last `days`. This is the
    /// savings surface: `savings_usd` is the money avoided by routing cheap
    /// tasks locally instead of escalating everything to frontier.
    pub fn summary(base_dir: &Path, days: u32) -> LedgerSummary {
        let events = Self::replay_days(base_dir, days);
        let total = events.len();
        let mut escalations = 0;
        let mut spend_usd = 0.0;
        let mut savings_usd = 0.0;
        for e in &events {
            if matches!(
                e.decision,
                mur_common::route::RouteDecision::Escalate { .. }
            ) {
                escalations += 1;
            }
            spend_usd += e.estimated_cost_usd;
            // Money avoided on tasks that stayed local.
            savings_usd += (e.counterfactual_cost_usd - e.estimated_cost_usd).max(0.0);
        }
        let rate = if total > 0 {
            escalations as f64 / total as f64
        } else {
            0.0
        };
        LedgerSummary {
            escalations,
            total,
            rate,
            spend_usd,
            savings_usd,
        }
    }
}

/// Aggregate escalation + cost KPIs over a window of ledger events.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LedgerSummary {
    /// Decisions that escalated to a frontier model.
    pub escalations: usize,
    /// Total routing decisions recorded.
    pub total: usize,
    /// `escalations / total` (0.0 when empty).
    pub rate: f64,
    /// Estimated USD actually spent on frontier escalations.
    pub spend_usd: f64,
    /// Estimated USD saved by routing cheap tasks locally
    /// (Σ counterfactual − Σ spend).
    pub savings_usd: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::route::{RouteDecision, TaskType};
    use tempfile::TempDir;

    fn make_event(escalate: bool) -> EscalationEvent {
        EscalationEvent {
            timestamp: "2026-06-01T12:00:00Z".into(),
            task_summary: "test task".into(),
            difficulty_score: if escalate { 0.82 } else { 0.15 },
            task_type: TaskType::General,
            estimated_context_tokens: 1000,
            decision: if escalate {
                RouteDecision::Escalate {
                    model_id: "anthropic_opus".into(),
                    reason: "high difficulty".into(),
                }
            } else {
                RouteDecision::Local {
                    model_id: "ollama_llama3".into(),
                    reason: "low difficulty".into(),
                }
            },
            role: None,
            escalation_from: if escalate {
                Some("ollama_llama3".into())
            } else {
                None
            },
            estimated_cost_usd: if escalate { 0.015 } else { 0.0 },
            counterfactual_cost_usd: 0.015,
        }
    }

    #[test]
    fn append_and_replay_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let mut ledger = EscalationLedger::open(tmp.path()).unwrap();
        ledger.append(&make_event(true)).unwrap();
        ledger.append(&make_event(false)).unwrap();
        ledger.flush().unwrap();
        drop(ledger);

        let events = EscalationLedger::replay_today(tmp.path());
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn open_creates_missing_dir() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("sub").join("deep");
        EscalationLedger::open(&sub).unwrap();
        assert!(sub.exists());
    }

    #[test]
    fn escalation_rate_computes_correctly() {
        let tmp = TempDir::new().unwrap();
        let mut ledger = EscalationLedger::open(tmp.path()).unwrap();
        // 3 local, 2 escalate → rate = 2/5 = 0.4
        ledger.append(&make_event(false)).unwrap(); // local
        ledger.append(&make_event(true)).unwrap(); // escalate
        ledger.append(&make_event(false)).unwrap(); // local
        ledger.append(&make_event(false)).unwrap(); // local
        ledger.append(&make_event(true)).unwrap(); // escalate
        ledger.flush().unwrap();
        drop(ledger);

        let (esc, total, rate) = EscalationLedger::escalation_rate(tmp.path(), 1);
        assert_eq!(esc, 2);
        assert_eq!(total, 5);
        assert!((rate - 0.4).abs() < 0.001, "rate={rate}, expected 0.4");
    }

    #[test]
    fn empty_ledger_rate_is_zero() {
        let tmp = TempDir::new().unwrap();
        let (_esc, total, rate) = EscalationLedger::escalation_rate(tmp.path(), 7);
        assert_eq!(total, 0);
        assert_eq!(rate, 0.0);
    }

    #[test]
    fn summary_reports_spend_and_savings() {
        let tmp = TempDir::new().unwrap();
        let mut ledger = EscalationLedger::open(tmp.path()).unwrap();
        // 3 local (each avoids $0.015), 2 escalate (each spends $0.015).
        for escalate in [false, true, false, false, true] {
            ledger.append(&make_event(escalate)).unwrap();
        }
        ledger.flush().unwrap();
        drop(ledger);

        let s = EscalationLedger::summary(tmp.path(), 1);
        assert_eq!(s.escalations, 2);
        assert_eq!(s.total, 5);
        assert!((s.spend_usd - 0.030).abs() < 1e-9, "spend={}", s.spend_usd);
        assert!(
            (s.savings_usd - 0.045).abs() < 1e-9,
            "savings={}",
            s.savings_usd
        );
    }
}
