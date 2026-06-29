//! Greedy best-score selection per unit across tracks.
//! Tie-breaking: first track wins (keeps stable ordering).

use crate::parallel::judge::TrackScore;
use super::{CherryPlan, UnitSelection};
use std::collections::HashMap;

/// Select the highest-scoring track for each unit.
/// On ties, the first track (by order in the input) wins.
pub fn cherry_pick(scores_per_unit: &[(&str, Vec<TrackScore>)]) -> CherryPlan {
    let mut selections = HashMap::new();
    for (unit_name, track_scores) in scores_per_unit {
        let best = track_scores.iter().fold(None::<&TrackScore>, |best, ts| {
            Some(match best {
                None => ts,
                Some(b) if ts.score > b.score => ts,
                Some(b) => b,
            })
        });
        let Some(best) = best else { continue };
        selections.insert(unit_name.to_string(), UnitSelection {
            unit_name: unit_name.to_string(),
            winning_track: best.track_name.clone(),
            score: best.score,
            low_confidence: best.low_confidence,
        });
    }
    CherryPlan { selections }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parallel::judge::TrackScore;

    fn ts(track: &str, score: f32) -> TrackScore {
        TrackScore { track_name: track.into(), score, reasoning: String::new(), low_confidence: false }
    }

    #[test]
    fn picks_highest_score_per_unit() {
        let scores_per_unit: Vec<(&str, Vec<TrackScore>)> = vec![
            ("authenticate", vec![ts("track-a", 9.0), ts("track-b", 7.5)]),
            ("logout", vec![ts("track-a", 6.0), ts("track-b", 8.5)]),
        ];
        let plan = cherry_pick(&scores_per_unit);
        assert_eq!(plan.winning_track_for("authenticate").unwrap(), "track-a");
        assert_eq!(plan.winning_track_for("logout").unwrap(), "track-b");
    }

    #[test]
    fn tie_goes_to_first_track() {
        let scores: Vec<(&str, Vec<TrackScore>)> = vec![
            ("f", vec![ts("track-a", 8.0), ts("track-b", 8.0)]),
        ];
        let plan = cherry_pick(&scores);
        assert_eq!(plan.winning_track_for("f").unwrap(), "track-a");
    }
}
