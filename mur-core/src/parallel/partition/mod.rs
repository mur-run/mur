#![allow(dead_code, unused_imports)]
//! Semantic Partition Mode (P2.5): split one file into disjoint regions,
//! assign each region to a different agent, merge results deterministically.

pub mod merge;
pub mod planner;
pub mod prompt;

/// One agent's slice of a file: the top-level unit names it owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionAssignment {
    pub track_name: String,
    pub unit_names: Vec<String>,
}

/// Full assignment of every region to exactly one agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionPlan {
    pub assignments: Vec<RegionAssignment>,
}

impl PartitionPlan {
    /// Which track owns `unit_name`, if any.
    pub fn owner_of(&self, unit_name: &str) -> Option<&str> {
        self.assignments
            .iter()
            .find(|a| a.unit_names.iter().any(|u| u == unit_name))
            .map(|a| a.track_name.as_str())
    }
}
