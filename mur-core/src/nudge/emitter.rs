use crate::nudge::candidate::WorkflowCandidate;
use crate::nudge::ledger::{NudgeLedger, NudgeState};
use anyhow::{anyhow, Result};
use chrono::{DateTime, Duration, Utc};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NudgeDecision {
    Accept,
    Dismiss,
    Snooze,
}

pub struct NudgeEmitter;

impl NudgeEmitter {
    /// Mark each actionable candidate Surfaced (records its snapshot).
    pub fn emit_pending(
        ledger: &mut NudgeLedger,
        actionable: &[WorkflowCandidate],
        now: DateTime<Utc>,
    ) {
        for c in actionable {
            ledger.mark_surfaced(c, now);
        }
    }

    /// Apply a user decision. `create` is called with the candidate on Accept
    /// (injected so the emitter stays free of workflow-store deps for testing).
    pub fn apply_decision(
        ledger: &mut NudgeLedger,
        id: &str,
        decision: NudgeDecision,
        snooze_days: u32,
        now: DateTime<Utc>,
        create: &dyn Fn(&WorkflowCandidate) -> Result<()>,
    ) -> Result<()> {
        match decision {
            NudgeDecision::Accept => {
                let cand = ledger
                    .get(id)
                    .and_then(|r| r.candidate.clone())
                    .ok_or_else(|| anyhow!("no pending nudge with id {id}"))?;
                create(&cand)?;
                ledger.set_state(id, NudgeState::Accepted, now);
            }
            NudgeDecision::Dismiss => ledger.set_state(id, NudgeState::Dismissed, now),
            NudgeDecision::Snooze => {
                let until = (now + Duration::days(snooze_days as i64)).to_rfc3339();
                ledger.set_state(id, NudgeState::Snoozed { until }, now);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nudge::candidate::WorkflowCandidate;
    use crate::nudge::ledger::{NudgeLedger, NudgeState};
    use chrono::Utc;

    fn cand(id: &str) -> WorkflowCandidate {
        WorkflowCandidate {
            id: id.into(),
            title: "Run tests then commit".into(),
            suggested_name: "test-then-commit".into(),
            steps_preview: vec![],
            session_count: 3,
            evidence_session_ids: vec!["s1".into()],
        }
    }

    #[test]
    fn emit_marks_surfaced() {
        let mut l = NudgeLedger::default();
        NudgeEmitter::emit_pending(&mut l, &[cand("a")], Utc::now());
        assert!(matches!(l.get("a").unwrap().state, NudgeState::Surfaced));
        assert_eq!(l.get("a").unwrap().surface_count, 1);
    }

    #[test]
    fn dismiss_decision_updates_ledger() {
        let mut l = NudgeLedger::default();
        NudgeEmitter::emit_pending(&mut l, &[cand("a")], Utc::now());
        NudgeEmitter::apply_decision(
            &mut l,
            "a",
            NudgeDecision::Dismiss,
            7,
            Utc::now(),
            &|_c| Ok(()),
        )
        .unwrap();
        assert!(matches!(l.get("a").unwrap().state, NudgeState::Dismissed));
    }

    #[test]
    fn accept_decision_calls_creator_and_marks_accepted() {
        let mut l = NudgeLedger::default();
        NudgeEmitter::emit_pending(&mut l, &[cand("a")], Utc::now());
        let created = std::cell::Cell::new(false);
        NudgeEmitter::apply_decision(
            &mut l,
            "a",
            NudgeDecision::Accept,
            7,
            Utc::now(),
            &|c| {
                assert_eq!(c.suggested_name, "test-then-commit");
                created.set(true);
                Ok(())
            },
        )
        .unwrap();
        assert!(created.get());
        assert!(matches!(l.get("a").unwrap().state, NudgeState::Accepted));
    }
}
