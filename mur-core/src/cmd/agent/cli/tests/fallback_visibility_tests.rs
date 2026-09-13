//! `fallback_visibility_tests`, moved out of `cli/mod.rs` for CLAUDE.md §4's 800-line rule.
//! Pure movement: dedented one level, nothing else.
//!
//! #947: the substitution must reach the user — a silent downgrade changes both
//! cost and quality with nothing on screen to show it happened.

use super::super::PricingBook;
use super::super::app::{App, Role};
use super::super::footer::Pricing;
use std::collections::HashMap;

fn app_with_book() -> App {
    let mut by_key = HashMap::new();
    let mut key_of_model = HashMap::new();
    // Configured: a local model with no price at all.
    by_key.insert("omlx".to_string(), Pricing::default());
    key_of_model.insert("Qwen3.5-4B-MLX-4bit".to_string(), "omlx".to_string());
    // The global fallback: a metered cloud model.
    by_key.insert(
        "claude_haiku".to_string(),
        Pricing {
            in_per_1k: Some(0.001),
            out_per_1k: Some(0.005),
            window: Some(200_000),
        },
    );
    key_of_model.insert("claude-haiku-4-5".to_string(), "claude_haiku".to_string());

    let mut a = App::test_fixture();
    a.pricing_book = Some(PricingBook {
        by_key,
        key_of_model,
        configured_key: Some("omlx".to_string()),
    });
    a.pricing = Pricing::default();
    a
}

fn usage(model: &str) -> serde_json::Value {
    serde_json::json!({ "input_tokens": 1000, "output_tokens": 1000, "model_ref": model })
}

fn notices(a: &App) -> usize {
    a.messages
        .iter()
        .filter(|m| m.role == Role::System && m.text.contains("fell back"))
        .count()
}

/// The exact production shape: a local free model 404s, the chain
/// substitutes a metered cloud one, and the footer used to keep pricing it
/// as the local model — showing "no price" for a turn that cost money.
#[test]
fn a_substituted_model_reprices_the_turn_and_says_so() {
    let mut a = app_with_book();
    assert!(
        a.pricing.in_per_1k.is_none(),
        "configured model is unpriced"
    );

    a.apply_usage(&usage("claude-haiku-4-5-20251001"));

    assert_eq!(
        a.pricing.in_per_1k,
        Some(0.001),
        "must re-price from the model that actually answered"
    );
    assert_eq!(notices(&a), 1, "the substitution must be announced");
}

/// Announced on the change, not on every turn — a long session that fell
/// back once must not repeat itself forever.
#[test]
fn the_notice_does_not_repeat_while_the_substitution_holds() {
    let mut a = app_with_book();
    for _ in 0..4 {
        a.apply_usage(&usage("claude-haiku-4-5"));
    }
    assert_eq!(notices(&a), 1);
}

/// Control: answering with the configured model says nothing at all.
#[test]
fn the_configured_model_answering_is_not_an_event() {
    let mut a = app_with_book();
    a.apply_usage(&usage("Qwen3.5-4B-MLX-4bit"));
    assert_eq!(notices(&a), 0);
    assert!(a.pricing.in_per_1k.is_none());
}

/// A model the registry cannot price must show "unknown", never inherit the
/// configured model's rates — that is the wrong-number bug in miniature.
#[test]
fn an_unpriceable_answer_does_not_inherit_the_configured_rates() {
    let mut a = app_with_book();
    a.pricing = Pricing {
        in_per_1k: Some(9.99),
        out_per_1k: Some(9.99),
        window: Some(1),
    };
    a.apply_usage(&usage("some-model-nobody-registered"));
    assert!(
        a.pricing.in_per_1k.is_none(),
        "stale rates must not survive an unpriceable answer"
    );
}
