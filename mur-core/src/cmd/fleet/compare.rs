//! `mur fleet compare <name>` — show per-unit scores across all tracks.

use anyhow::{Context, Result};
use std::path::Path;

use super::store::load_fleet;
use crate::parallel::{
    semantic::{SupportedLanguage, extract_units},
    state::ParallelStateDb,
    track::TrackSet,
};

pub fn cmd_fleet_compare(
    mur_home: &Path,
    fleet_name: &str,
    unit_filter: Option<&str>,
) -> Result<()> {
    let fleet = load_fleet(mur_home, fleet_name)?;
    let parallel = fleet
        .parallel
        .as_ref()
        .context("fleet has no parallel config")?;

    let fleet_dir = mur_home.join("fleets").join(fleet_name);
    let tracks = TrackSet::load(&fleet_dir)
        .context("no tracks.json — run `mur fleet run` then `mur fleet judge` first")?;
    let state_db = ParallelStateDb::open(&fleet_dir.join("parallel_state"))?;
    let rubric_ver = parallel.judge.rubric.version();

    let track_names: Vec<&str> = tracks
        .tracks
        .iter()
        .map(|t| t.config.name.as_str())
        .collect();

    type ScoreEntry = (String, Option<(f32, bool)>);
    // unit_name → Vec<(track_name, Option<(score, low_confidence)>)>
    let mut score_map: std::collections::HashMap<String, Vec<ScoreEntry>> =
        std::collections::HashMap::new();

    // Pass 1: collect (track_name, content_hash) per unit.name across all
    // tracks. We cannot look up scores yet — the competitor set for a unit
    // is only known once every track's implementation has been collected
    // (issue #545: CyclicJudge scores are relative to the competitor set).
    let mut by_name: std::collections::BTreeMap<String, Vec<(String, [u8; 32])>> =
        std::collections::BTreeMap::new();

    for t in &tracks.tracks {
        for entry in walkdir::WalkDir::new(&t.worktree_path)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("rs"))
        {
            let Ok(source) = std::fs::read(entry.path()) else {
                continue;
            };
            let Ok(units) = extract_units(&source, SupportedLanguage::Rust) else {
                continue;
            };
            for unit in units {
                if unit_filter.is_some_and(|f| !unit.name.contains(f)) {
                    continue;
                }
                by_name
                    .entry(unit.name)
                    .or_default()
                    .push((t.config.name.clone(), unit.content_hash));
            }
        }
    }

    // Pass 2: now that the full competitor set per unit is known, compute
    // its set hash and look up each track's score against that exact set.
    for (name, members) in &by_name {
        let set_hash =
            ParallelStateDb::competitor_set_hash(&members.iter().map(|m| m.1).collect::<Vec<_>>());
        for (track, hash) in members {
            let val = state_db
                .get_score(hash, &set_hash, &rubric_ver)
                .ok()
                .flatten()
                .map(|js| (js.score, js.low_confidence));
            score_map
                .entry(name.clone())
                .or_default()
                .push((track.clone(), val));
        }
    }

    if score_map.is_empty() {
        println!("No scores found. Run `mur fleet judge {fleet_name}` first.");
        return Ok(());
    }

    let col_w = 14usize;
    let name_w = 28usize;
    let sep = "─".repeat(name_w + (col_w + 1) * track_names.len() + 12);

    // Header
    print!("{:<name_w$}", "unit");
    for tn in &track_names {
        print!(" {:<col_w$}", tn);
    }
    println!(" winner");
    println!("{sep}");

    // Rows
    let mut unit_names: Vec<&String> = score_map.keys().collect();
    unit_names.sort();

    let mut track_sum = vec![0f32; track_names.len()];
    let mut track_cnt = vec![0usize; track_names.len()];
    let mut overall_winner: Option<usize> = None; // index into track_names

    for name in &unit_names {
        let by_track = score_map.get(*name).unwrap();
        let scores: Vec<Option<(f32, bool)>> = track_names
            .iter()
            .map(|tn| {
                by_track
                    .iter()
                    .find(|(t, _)| t.as_str() == *tn)
                    .and_then(|(_, v)| *v)
            })
            .collect();

        // Find winner index (highest score)
        let winner_idx = scores
            .iter()
            .enumerate()
            .filter_map(|(i, v)| v.map(|(s, _)| (i, s)))
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i);

        // Detect tie (two or more tracks share max score)
        let is_tie = if let Some(wi) = winner_idx {
            let max_score = scores[wi].unwrap().0;
            scores
                .iter()
                .filter(|v| v.is_some_and(|(s, _)| s == max_score))
                .count()
                > 1
        } else {
            false
        };

        print!("{:<name_w$}", truncate(name, name_w));
        for (i, val) in scores.iter().enumerate() {
            let cell = match val {
                Some((s, true)) => format!("{:.1} ⚠", s),
                Some((s, false)) => format!("{:.1}", s),
                None => "-".into(),
            };
            print!(" {:<col_w$}", cell);

            // accumulate averages
            if let Some((s, _)) = val {
                track_sum[i] += s;
                track_cnt[i] += 1;
            }
        }

        let winner_label = if is_tie {
            "(tie)".into()
        } else {
            winner_idx
                .map(|i| format!("{} ✓", track_names[i]))
                .unwrap_or_else(|| "-".into())
        };
        println!(" {winner_label}");
    }

    // Summary row
    println!("{sep}");
    print!("{:<name_w$}", "average");
    let mut best_avg = f32::NEG_INFINITY;
    for i in 0..track_names.len() {
        let avg = if track_cnt[i] > 0 {
            track_sum[i] / track_cnt[i] as f32
        } else {
            0.0
        };
        if avg > best_avg {
            best_avg = avg;
            overall_winner = Some(i);
        }
        print!(" {:<col_w$}", format!("{:.2}", avg));
    }
    // Tie on averages?
    let avg_winners: Vec<usize> = (0..track_names.len())
        .filter(|&i| {
            let avg = if track_cnt[i] > 0 {
                track_sum[i] / track_cnt[i] as f32
            } else {
                0.0
            };
            (avg - best_avg).abs() < 0.005
        })
        .collect();
    let overall_label = if avg_winners.len() > 1 {
        "(tie)".into()
    } else {
        overall_winner
            .map(|i| format!("{} ✓", track_names[i]))
            .unwrap_or_else(|| "-".into())
    };
    println!(" {overall_label}");

    Ok(())
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max { s } else { &s[..max] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long_string_clips() {
        assert_eq!(truncate("hello world", 5), "hello");
    }
}
