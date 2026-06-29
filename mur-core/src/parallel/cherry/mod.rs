#![allow(dead_code, unused_imports)]
pub mod assemble;
pub mod conflict;
pub mod picker;

use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct UnitSelection {
    pub unit_name: String,
    pub winning_track: String,
    pub score: f32,
    pub low_confidence: bool,
}

#[derive(Debug, Clone)]
pub struct CherryPlan {
    /// unit_name → winning track name
    pub selections: HashMap<String, UnitSelection>,
}

impl CherryPlan {
    pub fn winning_track_for(&self, unit_name: &str) -> Option<&str> {
        self.selections
            .get(unit_name)
            .map(|s| s.winning_track.as_str())
    }
}
