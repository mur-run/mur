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

    // unit_name → Vec<(track_name, Option<score>)>
    let mut score_map: std::collections::HashMap<String, Vec<(String, Option<f32>)>> =
        std::collections::HashMap::new();

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
                let score_val = state_db
                    .get_score(&unit.content_hash, &rubric_ver)
                    .ok()
                    .flatten()
                    .map(|js| js.score);
                score_map
                    .entry(unit.name)
                    .or_default()
                    .push((t.config.name.clone(), score_val));
            }
        }
    }

    if score_map.is_empty() {
        println!("No scores found. Run `mur fleet judge {fleet_name}` first.");
        return Ok(());
    }

    // Print header
    let col_w = 14usize;
    let name_w = 28usize;
    print!("{:<name_w$}", "Unit");
    for tn in &track_names {
        print!(" {:<col_w$}", tn);
    }
    println!(" Rec");
    println!(
        "{}",
        "-".repeat(name_w + (col_w + 1) * track_names.len() + 6)
    );

    // Print one row per unit
    let mut unit_names: Vec<&String> = score_map.keys().collect();
    unit_names.sort();
    for name in unit_names {
        let by_track = score_map.get(name).unwrap();
        let scores: Vec<(&str, Option<f32>)> = track_names
            .iter()
            .map(|tn| {
                let score = by_track
                    .iter()
                    .find(|(t, _)| t.as_str() == *tn)
                    .and_then(|(_, s)| *s);
                (*tn, score)
            })
            .collect();

        let rec = scores
            .iter()
            .filter_map(|(tn, s)| s.map(|v| (tn, v)))
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(tn, _)| *tn)
            .unwrap_or("-");

        print!("{:<name_w$}", truncate(name, name_w));
        for (_, score) in &scores {
            let cell = score
                .map(|s| format!("{:.1}", s))
                .unwrap_or_else(|| "-".into());
            print!(" {:<col_w$}", cell);
        }
        println!(" {rec} ★");
    }

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
