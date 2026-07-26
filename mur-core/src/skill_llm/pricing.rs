//! Per-1M-token USD rates for skill-LLM budget estimation.
//! Outdated rates → over-/under-estimate budget, never block correctness.

use mur_common::model::ModelEntry;

/// `(input, output)` per 1M tokens for a registry entry.
///
/// The user's own rate wins — `mur model add` records one from the models.dev
/// catalog, and until now nothing on this path read it, so a model MUR had
/// never heard of was budgeted from a family guess while its real price sat in
/// the registry. An entry priced on only one side is treated as unpriced
/// rather than half-believed.
pub fn rates_for(entry: &ModelEntry) -> (f64, f64) {
    // The registry is per-1k and this module is per-1M.
    if let (Some(input), Some(output)) = entry.effective_costs() {
        return (input * 1000.0, output * 1000.0);
    }
    rates(&entry.provider, &entry.model)
}

/// (input_usd_per_1m, output_usd_per_1m)
///
/// Published per-model rates live in exactly one place — the cost-report price
/// table. The copy that used to live here drifted a full model generation (it
/// was still billing Opus at $15/$75 and Gemini Flash at $0.15/$0.60), so all
/// this does now is guess a family rate for models that table has no row for.
pub fn rates(provider: &str, model: &str) -> (f64, f64) {
    if let Some((input, output, _, _)) = crate::cmd::conversations_cost_report::price_table(model) {
        return (input, output);
    }
    // Ids can arrive provider-prefixed ("anthropic/claude-opus-5") and miss the
    // table's `starts_with` match, so the family guess still keys off the name.
    match provider {
        "anthropic" if model.contains("opus") => (5.0, 25.0),
        "anthropic" if model.contains("haiku") => (1.0, 5.0),
        "anthropic" => (3.0, 15.0),
        "gemini" if model.contains("pro") => (2.0, 12.0),
        "gemini" => (1.50, 7.50),
        "openai" => (2.50, 15.0),
        "openrouter" => (3.0, 15.0),
        "ollama" => (0.0, 0.0),
        _ => (3.0, 15.0),
    }
}

/// Cheap heuristic: char-count / 4 → estimated tokens.
#[allow(dead_code)]
pub fn estimate_tokens(text: &str) -> u32 {
    (text.chars().count() as f64 / 4.0).ceil() as u32
}

/// Estimate cost in USD for a registry entry + token counts.
pub fn estimate_cost(entry: &ModelEntry, input_tokens: u32, output_tokens: u32) -> f64 {
    let (in_rate, out_rate) = rates_for(entry);
    (input_tokens as f64 / 1_000_000.0) * in_rate + (output_tokens as f64 / 1_000_000.0) * out_rate
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two hardcoded price tables drifted apart once already — this one still
    /// billed Opus at $15/$75 after the cost report was corrected to $5/$25.
    #[test]
    fn rates_agree_with_the_cost_report_table() {
        for (provider, model) in [
            ("anthropic", "claude-opus-5"),
            ("anthropic", "claude-haiku-4-5-20251001"),
            ("gemini", "gemini-3.6-flash"),
            ("openai", "gpt-5.6-sol"),
        ] {
            let (i, o, _, _) =
                crate::cmd::conversations_cost_report::price_table(model).expect("priced");
            assert_eq!(rates(provider, model), (i, o), "{model}");
        }
    }

    /// Provider-prefixed ids miss the table's `starts_with`, so the family
    /// fallback still has to put Opus above Sonnet rather than at the default.
    #[test]
    fn unlisted_ids_fall_back_to_a_family_rate() {
        assert_eq!(rates("anthropic", "anthropic/claude-opus-5"), (5.0, 25.0));
        assert_eq!(rates("ollama", "qwen3.5:4b"), (0.0, 0.0));
    }

    fn entry(provider: &str, model: &str, input: Option<f64>, output: Option<f64>) -> ModelEntry {
        ModelEntry {
            provider: provider.into(),
            model: model.into(),
            input_cost_per_1k: input,
            output_cost_per_1k: output,
            ..Default::default()
        }
    }

    /// The registry is per-1k and this module is per-1M. Getting the conversion
    /// backwards is a 1000x budget error that no eyeball catches.
    #[test]
    fn registry_rate_wins_and_converts_per_1k_to_per_1m() {
        let e = entry("openai", "vendor-x", Some(0.002), Some(0.008));
        assert_eq!(rates_for(&e), (2.0, 8.0));
        // 1M in + 1M out at those rates = $10.
        assert!((estimate_cost(&e, 1_000_000, 1_000_000) - 10.0).abs() < 1e-9);
    }

    /// A model MUR has never heard of is exactly the case the registry exists
    /// for — before this it was budgeted from a provider guess while its real
    /// rate sat one lookup away.
    #[test]
    fn unknown_model_uses_its_registry_rate_not_the_family_guess() {
        let e = entry("openai", "some-vendor-model-v9", Some(0.01), Some(0.04));
        assert_eq!(rates_for(&e), (10.0, 40.0));
        assert_ne!(rates_for(&e), rates("openai", "some-vendor-model-v9"));
    }

    /// Half a price is not a price: a one-sided entry falls back rather than
    /// pairing a real rate with a zero.
    #[test]
    fn half_priced_entry_falls_back_instead_of_pairing_with_zero() {
        let e = entry("anthropic", "claude-opus-5", Some(0.005), None);
        assert_eq!(rates_for(&e), (5.0, 25.0)); // from the table, not (5.0, 0.0)
    }
}
