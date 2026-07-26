//! Hard-coded per-provider USD / 1M-token rates for budget estimation.
//! Outdated rates → over-/under-estimate budget, never block correctness.

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

/// Estimate cost in USD for a provider + model + token counts.
pub fn estimate_cost(provider: &str, model: &str, input_tokens: u32, output_tokens: u32) -> f64 {
    let (in_rate, out_rate) = rates(provider, model);
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
}
