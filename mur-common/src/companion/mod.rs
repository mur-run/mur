//! Companion subsystem shared types (Phase 1.1).

pub mod content_seed;
pub mod voice_template;

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relationship {
    #[default]
    Friend,
    Coach,
    AccountabilityBuddy,
    Mentor,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Formality {
    Casual,
    #[default]
    Neutral,
    Formal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Situation {
    MorningGreeting,
    GentleCheckIn,
    ShareQuote,
    ShareLink,
    WorkflowNudge,
    /// A schedule the user set fired. Unlike every variant above, this one is
    /// never chosen by `situations::pick_for_hour` — the picker samples a fixed
    /// weight table and cannot reach it. It is supplied directly, because both
    /// questions the picker exists to answer (say something? say what?) were
    /// already answered by the person who wrote the schedule.
    Scheduled,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_nudge_situation_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&Situation::WorkflowNudge).unwrap(),
            "\"workflow_nudge\""
        );
        let back: Situation = serde_json::from_str("\"workflow_nudge\"").unwrap();
        assert_eq!(back, Situation::WorkflowNudge);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    Positive,
    Negative,
    Dismiss,
    Sent,
}
