//! Monitor runtime state and observed outcome are two different axes
//! (spec §狀態模型): the state says what the engine is doing with the
//! monitor, the outcome says what the source last told us about the work.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorState {
    Registering,
    Active,
    Checking,
    Sleeping,
    ActionPending,
    AwaitingApproval,
    Completed,
    Exhausted,
}

impl MonitorState {
    pub const ALL: &[MonitorState] = &[
        MonitorState::Registering,
        MonitorState::Active,
        MonitorState::Checking,
        MonitorState::Sleeping,
        MonitorState::ActionPending,
        MonitorState::AwaitingApproval,
        MonitorState::Completed,
        MonitorState::Exhausted,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            MonitorState::Registering => "registering",
            MonitorState::Active => "active",
            MonitorState::Checking => "checking",
            MonitorState::Sleeping => "sleeping",
            MonitorState::ActionPending => "action_pending",
            MonitorState::AwaitingApproval => "awaiting_approval",
            MonitorState::Completed => "completed",
            MonitorState::Exhausted => "exhausted",
        }
    }

    /// Accepts both `snake_case` (stored) and the spec's CLI kebab form.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.replace('-', "_");
        Self::ALL.iter().copied().find(|v| v.as_str() == s)
    }

    /// States the scheduler may claim. Everything else is parked for a
    /// human, an executor (plan-2), or is finished.
    pub fn is_claimable(self) -> bool {
        matches!(self, MonitorState::Active | MonitorState::Sleeping)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Pending,
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Pending => "pending",
            Outcome::Succeeded => "succeeded",
            Outcome::Failed => "failed",
            Outcome::Cancelled => "cancelled",
            Outcome::Unknown => "unknown",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Outcome::Pending),
            "succeeded" => Some(Outcome::Succeeded),
            "failed" => Some(Outcome::Failed),
            "cancelled" => Some(Outcome::Cancelled),
            "unknown" => Some(Outcome::Unknown),
            _ => None,
        }
    }
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Outcome::Succeeded | Outcome::Failed | Outcome::Cancelled
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_and_accepts_kebab() {
        for s in MonitorState::ALL {
            assert_eq!(MonitorState::parse(s.as_str()), Some(*s));
        }
        assert_eq!(
            MonitorState::parse("awaiting-approval"),
            Some(MonitorState::AwaitingApproval)
        );
        assert_eq!(MonitorState::parse("nope"), None);
    }

    #[test]
    fn only_three_outcomes_are_terminal() {
        assert!(Outcome::Succeeded.is_terminal());
        assert!(Outcome::Failed.is_terminal());
        assert!(Outcome::Cancelled.is_terminal());
        assert!(!Outcome::Pending.is_terminal());
        assert!(!Outcome::Unknown.is_terminal());
        assert_eq!(Outcome::parse("unknown"), Some(Outcome::Unknown));
    }
}
