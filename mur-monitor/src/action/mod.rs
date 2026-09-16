//! Action identity and lifecycle (spec §冪等與事件紀錄).

pub mod risk;

/// The stable claim key. Spec §冪等與事件紀錄, verbatim:
/// `<monitor-id>:<cycle-id>:<observed-terminal-version>:<action-type>:<action-index>`
///
/// `observed_terminal_version` is the monitor's fence when the terminal
/// observation was written. Including it means a re-observed terminal (a
/// child cycle after a remedy) claims afresh, while a daemon restart
/// replaying the same terminal collides with the row already there.
pub struct ActionKey;

impl ActionKey {
    pub fn new(
        monitor_id: &str,
        cycle_id: &str,
        observed_terminal_version: i64,
        action_type: &str,
        action_index: usize,
    ) -> String {
        format!("{monitor_id}:{cycle_id}:{observed_terminal_version}:{action_type}:{action_index}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionState {
    /// Claimed, not yet run.
    Claimed,
    /// Gated and parked: a human has not answered. NOT a failure.
    Blocked,
    Done,
    Failed,
}

impl ActionState {
    pub fn as_str(self) -> &'static str {
        match self {
            ActionState::Claimed => "claimed",
            ActionState::Blocked => "blocked",
            ActionState::Done => "done",
            ActionState::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "claimed" => Some(ActionState::Claimed),
            "blocked" => Some(ActionState::Blocked),
            "done" => Some(ActionState::Done),
            "failed" => Some(ActionState::Failed),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::hitl::RiskTier;

    #[test]
    fn the_key_is_the_specs_five_field_shape() {
        // spec §冪等與事件紀錄: <monitor-id>:<cycle-id>:<version>:<type>:<index>
        assert_eq!(
            ActionKey::new("m1", "c1", 7, "notify", 0),
            "m1:c1:7:notify:0"
        );
    }

    #[test]
    fn a_re_observed_terminal_gets_a_different_key() {
        // Same monitor, same cycle, same action — but a new terminal
        // observation (higher fence). A fresh claim must be possible, or a
        // child cycle after a remedy could never act.
        assert_ne!(
            ActionKey::new("m1", "c1", 7, "notify", 0),
            ActionKey::new("m1", "c1", 8, "notify", 0)
        );
    }

    #[test]
    fn a_restart_replaying_the_same_terminal_gets_the_same_key() {
        // The whole point of the claim: this must collide so the unique
        // constraint refuses the second attempt.
        assert_eq!(
            ActionKey::new("m1", "c1", 7, "collect_logs", 2),
            ActionKey::new("m1", "c1", 7, "collect_logs", 2)
        );
    }

    #[test]
    fn every_known_action_has_a_tier_and_none_is_read_by_accident() {
        // A verb added to KNOWN_ACTIONS without a tier must not silently
        // become auto-executable. `classify` is total and its fallback is
        // the most restrictive tier, not the least.
        for a in mur_monitor::spec::KNOWN_ACTIONS {
            let t = risk::classify(a);
            if *a == "notify" || *a == "collect_logs" || *a == "reschedule_monitor" {
                assert_eq!(t, RiskTier::Read, "{a}");
            } else {
                assert!(t > RiskTier::Read, "{a} must not be auto-executable");
            }
        }
    }

    #[test]
    fn an_unknown_verb_classifies_as_privileged_not_read() {
        // The safety property: classification is total, and anything the
        // table does not name is the most restrictive tier. A verb that
        // slipped past spec validation must not become auto-executable by
        // being unrecognised.
        assert_eq!(
            risk::classify("definitely_not_a_verb"),
            RiskTier::Privileged
        );
        assert_eq!(risk::classify(""), RiskTier::Privileged);
    }

    #[test]
    fn start_downstream_is_not_read_even_though_it_runs_on_success() {
        // spec §風險政策 calls this out by name: 「成功就部署 production」
        // 不因寫在 success action 就自動成為低風險.
        assert!(risk::classify("start_downstream") > RiskTier::Read);
    }

    #[test]
    fn action_state_round_trips() {
        for s in [
            ActionState::Claimed,
            ActionState::Blocked,
            ActionState::Done,
            ActionState::Failed,
        ] {
            assert_eq!(ActionState::parse(s.as_str()), Some(s));
        }
        assert_eq!(ActionState::parse("nonsense"), None);
    }
}
