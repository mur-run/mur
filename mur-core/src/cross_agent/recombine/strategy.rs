//! Recombination strategies.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecombineStrategy {
    Union,
    Intersection,
    Llm,
}

impl RecombineStrategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            RecombineStrategy::Union => "union",
            RecombineStrategy::Intersection => "intersection",
            RecombineStrategy::Llm => "llm",
        }
    }
}

/// Tiebreak inputs for Intersection's per-step keeper selection.
#[derive(Debug, Clone)]
pub struct FitnessCtx {
    pub a_agent: String,
    pub b_agent: String,
    pub a_success_rate: f64,
    pub b_success_rate: f64,
    pub a_weight: f64,
    pub b_weight: f64,
}
