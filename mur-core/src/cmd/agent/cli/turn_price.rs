//! Pricing one finished turn from its `Task.usage`. Shared by the TUI
//! (`App::apply_usage`) and the `--plain` path so the two cannot drift: plain
//! mode used to price every turn at the configured model and ignore both
//! `model_ref` and `by_model` (qa review I3 on #2609).

use super::PricingBook;
use super::footer::{self, Pricing, UsageCounts, own_rates, unbilled_label};
use mur_common::model::BillingMode;

/// Pricing of the model that answered this turn (`usage.model_ref`, the LAST
/// call's provider model name). `None` when the runtime did not say, or there
/// is no book to look it up in; an answering model the registry does not know
/// is unpriced (`Pricing::default()`) rather than guessed at.
pub(super) fn answered_pricing(
    book: Option<&PricingBook>,
    usage: &serde_json::Value,
) -> Option<Pricing> {
    let book = book?;
    let answered = usage.get("model_ref").and_then(|v| v.as_str())?;
    Some(
        book.key_for_model(answered)
            .map(|k| book.pricing_for_key(k))
            .unwrap_or_default(),
    )
}

/// One turn's price, plus the models whose tokens were counted at the book's
/// dearest rates because the registry could not resolve or price them. The
/// figure is then an upper estimate, and the footer has to say so — the fleet
/// guard does the same with `warn_unpriced` (#2620). Decided in the same match
/// as the figure so the two cannot drift.
pub(super) struct TurnPrice {
    pub usd: Option<f64>,
    /// Sorted, deduplicated; empty when nothing was guessed at.
    pub guessed: Vec<String>,
}

/// Price one turn. A turn is many calls and fallback can answer each with a
/// different model, so each `by_model` share is priced at its own model (qa
/// review C1). Older runtimes without the split are priced at `pricing`, the
/// answering model's rates — unless the registry cannot resolve the answering
/// model, which is counted at the book's dearest rates exactly like an
/// unresolvable share, so a legacy runtime cannot reopen the "unknown reads as
/// free" hole. A metered answering model the registry knows but carries no
/// unit price for is counted at the dearest rates too, as an unpriced share
/// is — so a turn costs the same on either runtime. `usd: None` = nothing in
/// the turn is billed per token.
pub(super) fn price_turn(
    pricing: &Pricing,
    book: Option<&PricingBook>,
    usage: &serde_json::Value,
    u: &UsageCounts,
) -> TurnPrice {
    let answered = || {
        let model = usage.get("model_ref").and_then(|v| v.as_str());
        let key = match (book, model) {
            (Some(b), Some(m)) => b.key_for_model(m).map(str::to_string),
            (Some(b), None) => b.configured_key.clone(),
            (None, _) => None,
        };
        display_name(model, key.as_deref())
    };
    let guessed_if = |usd: Option<f64>| TurnPrice {
        guessed: if usd.is_some() && (u.input > 0 || u.output > 0) {
            vec![answered()]
        } else {
            Vec::new()
        },
        usd,
    };
    match (book, usage.get("by_model").and_then(|v| v.as_array())) {
        (Some(book), Some(shares)) if !shares.is_empty() => cost_by_share(book, shares),
        (Some(book), _) if answered_is_unresolved(book, usage) => {
            guessed_if(at_rates(book.dearest_rates(), u))
        }
        (Some(book), _) if pricing.billing == Some(BillingMode::UsageBilled) => {
            match own_rates(pricing) {
                Some(r) => TurnPrice {
                    usd: at_rates(Some(r), u),
                    guessed: Vec::new(),
                },
                None => guessed_if(at_rates(book.dearest_rates(), u)),
            }
        }
        _ => TurnPrice {
            usd: footer::turn_cost(pricing, u),
            guessed: Vec::new(),
        },
    }
}

/// What a guessed-at model is called when neither the share, the turn nor the
/// configuration names it.
const UNNAMED_MODEL: &str = "an unnamed model";

/// How a guessed-at model is named in the notice: the provider model id, plus
/// the registry key `mur model add` takes when it is known and differs — the
/// provider id alone is not always a valid key (qa review on #2623).
fn display_name(model: Option<&str>, key: Option<&str>) -> String {
    match (model, key) {
        (Some(m), Some(k)) if m != k => format!("{m} (key {k})"),
        (Some(m), _) => m.to_string(),
        (None, Some(k)) => k.to_string(),
        (None, None) => UNNAMED_MODEL.to_string(),
    }
}

/// `model_ref` is present but names nothing in the registry.
fn answered_is_unresolved(book: &PricingBook, usage: &serde_json::Value) -> bool {
    usage
        .get("model_ref")
        .and_then(|v| v.as_str())
        .is_some_and(|m| book.key_for_model(m).is_none())
}

/// USD for `u` at `(input, output)` per-1k rates; `None` without rates.
fn at_rates(rates: Option<(f64, f64)>, u: &UsageCounts) -> Option<f64> {
    let (i, o) = rates?;
    Some(u.input as f64 / 1000.0 * i + u.output as f64 / 1000.0 * o)
}

/// The `--plain` footer line for one turn: tokens, then the cost, or the
/// answering model's billing mode when nothing was billed per token. The book
/// is required, not optional: without it the turn is priced at the configured
/// model, which is exactly the plain-mode drift this module exists to close.
pub(super) fn plain_footer(
    configured: &Pricing,
    book: &PricingBook,
    usage: &serde_json::Value,
) -> String {
    let u = footer::parse_usage(usage);
    let pricing = answered_pricing(Some(book), usage).unwrap_or(*configured);
    let tok = u.input + u.output;
    let priced = price_turn(&pricing, Some(book), usage, &u);
    match priced.usd {
        Some(c) if !priced.guessed.is_empty() => format!(
            "  {tok} tok · {}${c:.3} upper estimate, no rate for {}",
            footer::UPPER_BOUND_MARK,
            priced.guessed.join(", ")
        ),
        Some(c) => format!("  {tok} tok · ${c:.3}"),
        None => match unbilled_label(&pricing) {
            Some(label) => format!("  {tok} tok · {label}"),
            None => format!("  {tok} tok"),
        },
    }
}

/// USD for one turn priced share by share. `None` when no share is billed per
/// token (an all-subscription or all-local turn has no money figure). A share
/// whose model the registry cannot resolve or price is counted at the book's
/// dearest rates — the cap fails closed on what it cannot price.
fn cost_by_share(book: &PricingBook, shares: &[serde_json::Value]) -> TurnPrice {
    let mut total: Option<f64> = None;
    let mut guessed = Vec::new();
    for share in shares {
        let tokens = |k: &str| share.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
        let u = UsageCounts {
            input: tokens("input_tokens"),
            output: tokens("output_tokens"),
        };
        if u.input == 0 && u.output == 0 {
            continue;
        }
        let (rates, guess) = match book.key_for_share(share).map(|k| book.pricing_for_key(&k)) {
            Some(p) if unbilled_label(&p).is_some() => continue,
            Some(p) => match own_rates(&p) {
                Some(r) => (Some(r), false),
                None => (book.dearest_rates(), true),
            },
            None => (book.dearest_rates(), true),
        };
        // Nothing in the registry carries a rate: there is no figure to give.
        let Some(c) = at_rates(rates, &u) else {
            continue;
        };
        if guess {
            guessed.push(share_name(book, share));
        }
        *total.get_or_insert(0.0) += c;
    }
    guessed.sort_unstable();
    guessed.dedup();
    TurnPrice {
        usd: total,
        guessed,
    }
}

/// The provider model a share names, with the registry key it resolves to (or
/// reports, when the registry does not know it).
fn share_name(book: &PricingBook, share: &serde_json::Value) -> String {
    let field = |k: &str| share.get(k).and_then(|v| v.as_str());
    let key = book
        .key_for_share(share)
        .or_else(|| field("registry_key").map(str::to_string));
    display_name(field("model"), key.as_deref())
}

#[cfg(test)]
#[path = "turn_price_tests.rs"]
mod tests;
