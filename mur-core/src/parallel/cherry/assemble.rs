//! Reconstruct source files from cherry-picked units.

use anyhow::Result;
use crate::parallel::semantic::{SupportedLanguage, extract_units};
use super::CherryPlan;

pub struct TrackSource<'a> {
    pub track_name: &'a str,
    pub source: &'a [u8],
}

/// Reconstruct file replacing each unit with its winning track's version.
/// Units with no selection in the cherry plan keep their original source.
pub fn assemble_file(
    base_source: &[u8],
    plan: &CherryPlan,
    tracks: &[TrackSource<'_>],
    lang: SupportedLanguage,
) -> Result<Vec<u8>> {
    let base_units = extract_units(base_source, lang)?;
    let mut out = base_source.to_vec();

    // Sort units by byte range in reverse order (highest offset first).
    // This ensures splice operations don't invalidate later ranges.
    let mut sorted_units = base_units.clone();
    sorted_units.sort_by_key(|u| std::cmp::Reverse(u.byte_range.start));

    for base_unit in sorted_units {
        let Some(selection) = plan.selections.get(&base_unit.name) else { continue };

        // Find the track source with the winning track name.
        let track_src = tracks
            .iter()
            .find(|t| t.track_name == selection.winning_track)
            .map(|t| t.source);

        let Some(track_src) = track_src else { continue };

        let track_units = extract_units(track_src, lang)?;
        let Some(replacement) = track_units.iter().find(|u| u.name == base_unit.name) else { continue };

        let new_code = &track_src[replacement.byte_range.clone()];
        out.splice(base_unit.byte_range.clone(), new_code.iter().copied());
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parallel::cherry::{CherryPlan, UnitSelection};
    use std::collections::HashMap;

    #[test]
    fn replaces_winning_unit() {
        let base = b"fn greet() { println!(\"hello\"); }\nfn farewell() { println!(\"bye\"); }\n";
        let track_b_src = b"fn greet() { println!(\"hi there!\"); }\nfn farewell() { println!(\"bye\"); }\n";

        let mut sels = HashMap::new();
        sels.insert("greet".into(), UnitSelection {
            unit_name: "greet".into(),
            winning_track: "track-b".into(),
            score: 9.0,
            low_confidence: false,
        });

        let plan = CherryPlan { selections: sels };
        let tracks = vec![TrackSource { track_name: "track-b", source: track_b_src }];

        let result = assemble_file(base, &plan, &tracks, SupportedLanguage::Rust).unwrap();
        let result_str = std::str::from_utf8(&result).unwrap();

        assert!(result_str.contains("hi there!"), "expected replacement: {result_str}");
        assert!(result_str.contains("farewell"), "expected farewell preserved: {result_str}");
    }
}
