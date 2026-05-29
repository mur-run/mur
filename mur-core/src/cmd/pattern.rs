use anyhow::Result;
use mur_common::pattern::*;

use crate::store::yaml::YamlStore;

#[allow(dead_code)]
pub(crate) fn cmd_search(query: &str) -> Result<()> {
    use crate::retrieve::gate::{Tier as GateTier, evaluate_query};
    use crate::retrieve::scoring::score_and_rank;

    // Adaptive gate
    let outcome = evaluate_query(query);
    if outcome.tier == GateTier::Skip {
        println!(
            "# Gate skipped: {} (score={:.2})",
            outcome.reasons.join(", "),
            outcome.score
        );
        return Ok(());
    }

    let store = YamlStore::default_store()?;
    let patterns = store.list_all()?;
    let results = score_and_rank(query, patterns);

    if results.is_empty() {
        println!("No patterns found for: {}", query);
        return Ok(());
    }

    println!("🔍 Found {} patterns for \"{}\":\n", results.len(), query);
    for sp in &results {
        let p = &sp.item;
        let tier_icon = match p.tier {
            Tier::Session => "📝",
            Tier::Project => "📁",
            Tier::Core => "⭐",
        };
        println!(
            "  {} {} (score: {:.3}, relevance: {:.3}, importance: {:.0}%)\n    {}",
            tier_icon,
            p.name,
            sp.score,
            sp.relevance,
            p.importance * 100.0,
            p.description
        );
    }

    Ok(())
}
