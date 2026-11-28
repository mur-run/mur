//! The registry → pricing book path, driven from `models.yaml` text (qa review
//! on #2609). Every other pricing test hand-builds `Pricing`, so a book that
//! dropped `billing` (or the rates) on the way in would leave them all green.

use super::super::pricing_book_from;
use mur_common::model::{BillingMode, ModelRegistry};

const REGISTRY: &str = r#"
schema_version: 1
models:
  api:
    provider: anthropic
    model: claude-haiku-4-5
    input_cost_per_1k: 0.001
    output_cost_per_1k: 0.005
    context_window: 200000
  plus:
    provider: codex
    model: gpt-5-codex
    input_cost_per_1k: 0.00125
    output_cost_per_1k: 0.01
  mlx:
    provider: ollama
    model: qwen3:4b
  gateway:
    provider: openai
    model: gpt-5
    billing: subscription
"#;

fn registry() -> ModelRegistry {
    serde_yaml_ng::from_str(REGISTRY).expect("fixture registry parses")
}

#[test]
fn billing_reaches_the_book_explicit_and_inferred() {
    let (_, book) = pricing_book_from(&registry(), None);
    for (key, want) in [
        ("api", BillingMode::UsageBilled),
        ("plus", BillingMode::Subscription),
        ("mlx", BillingMode::Local),
        // An explicit `billing:` beats the provider's inference (openai → metered).
        ("gateway", BillingMode::Subscription),
    ] {
        assert_eq!(book.pricing_for_key(key).billing, Some(want), "{key}");
    }
}

#[test]
fn rates_and_window_reach_the_book() {
    let (_, book) = pricing_book_from(&registry(), None);
    let p = book.pricing_for_key("api");
    assert_eq!(
        (p.in_per_1k, p.out_per_1k, p.window),
        (Some(0.001), Some(0.005), Some(200_000))
    );
}

#[test]
fn the_configured_key_prices_the_session_and_resolves_by_model() {
    let (current, book) = pricing_book_from(&registry(), Some("plus".to_string()));
    assert_eq!(current.billing, Some(BillingMode::Subscription));
    assert_eq!(book.key_for_model("gpt-5-codex"), Some("plus"));
    let (none, _) = pricing_book_from(&registry(), None);
    assert_eq!(none.billing, None, "no configured key is unknown, not free");
}
