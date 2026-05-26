//! Hard-coded per-provider USD / 1M-token rates for budget estimation.
//! Outdated rates → over-/under-estimate budget, never block correctness.

/// (input_usd_per_1m, output_usd_per_1m)
pub fn rates(provider: &str, model: &str) -> (f64, f64) {
    match provider {
        "anthropic" => match () {
            _ if model.contains("opus") => (15.0, 75.0),
            _ if model.contains("sonnet") => (3.0, 15.0),
            _ if model.contains("haiku") => (0.80, 4.0),
            _ => (3.0, 15.0),
        },
        "openai" => match () {
            _ if model.contains("gpt-4o") && !model.contains("mini") => (2.50, 10.0),
            _ if model.contains("gpt-4o-mini") => (0.15, 0.60),
            _ if model.contains("o1") || model.contains("o3") => (15.0, 60.0),
            _ => (2.50, 10.0),
        },
        "gemini" => match () {
            _ if model.contains("pro") => (1.25, 10.0),
            _ if model.contains("flash") => (0.15, 0.60),
            _ => (1.25, 10.0),
        },
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
