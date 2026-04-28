//! Companion subsystem shared types (Phase 1.1).

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    Positive,
    Negative,
    Dismiss,
    Sent,
}
