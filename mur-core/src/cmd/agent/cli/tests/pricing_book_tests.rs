//! `pricing_book_tests`, moved out of `cli/mod.rs` for CLAUDE.md §4's 800-line rule.
//! Pure movement: dedented one level, nothing else.
//!
//! #947: pricing used to be resolved once at startup from the configured
//! `model_ref`. The runtime substitutes a different model whenever a candidate
//! is unreachable, out of credit, or serving a retired id — and the footer went
//! on charging at the configured rates.

use super::super::PricingBook;
use super::super::footer::Pricing;
use std::collections::HashMap;

fn book() -> PricingBook {
    let mut by_key = HashMap::new();
    let mut key_of_model = HashMap::new();
    by_key.insert(
        "omlx".to_string(),
        Pricing {
            in_per_1k: None,
            out_per_1k: None,
            window: Some(32_000),
        },
    );
    key_of_model.insert("Qwen3.5-4B-MLX-4bit".to_string(), "omlx".to_string());
    by_key.insert(
        "claude_haiku".to_string(),
        Pricing {
            in_per_1k: Some(0.001),
            out_per_1k: Some(0.005),
            window: Some(200_000),
        },
    );
    key_of_model.insert("claude-haiku-4-5".to_string(), "claude_haiku".to_string());
    // A newer id sharing the older one's prefix — the shadowing trap.
    by_key.insert(
        "claude_haiku_5".to_string(),
        Pricing {
            in_per_1k: Some(0.002),
            out_per_1k: Some(0.01),
            window: Some(400_000),
        },
    );
    key_of_model.insert(
        "claude-haiku-4-5-turbo".to_string(),
        "claude_haiku_5".to_string(),
    );
    PricingBook {
        by_key,
        key_of_model,
        configured_key: Some("omlx".to_string()),
    }
}

#[test]
fn a_provider_build_suffix_still_resolves_to_its_entry() {
    let b = book();
    // Exact.
    assert_eq!(b.key_for_model("claude-haiku-4-5"), Some("claude_haiku"));
    // Providers append a build date; the entry must still be found.
    assert_eq!(
        b.key_for_model("claude-haiku-4-5-20251001"),
        Some("claude_haiku")
    );
}

/// Longest prefix wins. A shorter, older id must not shadow a newer one
/// that shares its prefix — that mis-prices every call to the new model.
#[test]
fn the_longest_matching_id_wins_not_the_first() {
    let b = book();
    assert_eq!(
        b.key_for_model("claude-haiku-4-5-turbo-20260101"),
        Some("claude_haiku_5")
    );
}

#[test]
fn an_unknown_model_resolves_to_nothing_rather_than_the_nearest_guess() {
    assert_eq!(book().key_for_model("gpt-9-unreleased"), None);
}
