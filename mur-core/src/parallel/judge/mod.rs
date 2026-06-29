pub mod cyclic;
pub mod rubric;

use crate::parallel::semantic::SemanticUnit;
use mur_common::parallel::TrackConfig;

pub struct JudgeTask {
    pub unit_name: String,
    pub implementations: Vec<TrackImpl>,
    pub rubric_version: String,
}

pub struct TrackImpl {
    pub track_config: TrackConfig,
    pub unit: SemanticUnit,
    pub source: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct TrackScore {
    pub track_name: String,
    pub score: f32,
    pub reasoning: String,
    pub low_confidence: bool, // |round1 - round2| > 0.2
}

pub use cyclic::CyclicJudge;
