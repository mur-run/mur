//! Greedy best-score selection per unit across tracks.
//! Tie-breaking: first track wins (keeps stable ordering).

use crate::parallel::judge::TrackScore;
use super::{CherryPlan, UnitSelection};
use std::collections::HashMap;

/// Select the highest-scoring track for each unit.
/// On ties, the first track (by order in the input) wins.
pub fn cherry_pick(scores: &[TrackScore], unit_names: &[String]) -> CherryPlan {
    let mut selections: HashMap<String, UnitSelection> = HashMap::new();

    for unit_name in unit_names {
        // Find all scores for this unit across all tracks.
        let unit_scores: Vec<_> = scores.iter().filter(|s| &s.track_name == unit_name).collect();

        if unit_scores.is_empty() {
            continue;
        }

        // Use fold to pick the best score, with tie-breaking: first wins.
        // This is important: max_by returns the LAST equal element on ties,
        // but we want the FIRST element to win (stable ordering).
        let best = unit_scores.iter().fold(None, |best: Option<&&TrackScore>, ts| {
            Some(match best {
                None => ts,
                Some(b) if ts.score > b.score => ts,
                Some(b) => b,
            })
        });

        let Some(best) = best else { continue };

        selections.insert(unit_name.clone(), UnitSelection {
            unit_name: unit_name.clone(),
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

    fn ts(track: &str, score: f32) -> TrackScore {
        TrackScore {
            track_name: track.into(),
            score,
            reasoning: String::new(),
            low_confidence: false,
        }
    }

    #[test]
    fn picks_highest_score_per_unit() {
        let scores = vec![
            ts("track-a", 5.0),
            ts("track-b", 8.0),
            ts("track-a", 9.0),
        ];
        let units = vec!["track-a".into(), "track-b".into()];
        let plan = cherry_pick(&scores, &units);

        // track-a: max(5.0, 9.0) = 9.0
        // track-b: 8.0
        assert_eq!(plan.winning_track_for("track-a"), Some("track-a"));
        assert_eq!(plan.winning_track_for("track-b"), Some("track-b"));
    }

    #[test]
    fn tie_goes_to_first_track() {
        // Two scores, same value; first should win (stable).
        let scores = vec![
            ts("track-a", 7.0),
            ts("track-b", 7.0),
        ];
        let units = vec!["track-a".into(), "track-b".into()];
        let plan = cherry_pick(&scores, &units);

        // On tie (7.0 == 7.0), track-a appears first, so it wins.
        assert_eq!(plan.winning_track_for("track-a"), Some("track-a"));
        assert_eq!(plan.winning_track_for("track-b"), Some("track-b"));
    }
}
