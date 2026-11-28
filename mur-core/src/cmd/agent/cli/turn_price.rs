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

/// USD for one turn. A turn is many calls and fallback can answer each with a
/// different model, so each `by_model` share is priced at its own model (qa
/// review C1). Older runtimes without the split are priced at `pricing`, the
/// answering model's rates — unless the registry cannot resolve the answering
/// model, which is counted at the book's dearest rates exactly like an
/// unresolvable share, so a legacy runtime cannot reopen the "unknown reads as
/// free" hole. A metered answering model the registry knows but carries no
/// unit price for is counted at the dearest rates too, as an unpriced share
/// is — so a turn costs the same on either runtime. `None` = nothing in the
/// turn is billed per token.
pub(super) fn turn_cost(
    pricing: &Pricing,
    book: Option<&PricingBook>,
    usage: &serde_json::Value,
    u: &UsageCounts,
) -> Option<f64> {
    match (book, usage.get("by_model").and_then(|v| v.as_array())) {
        (Some(book), Some(shares)) if !shares.is_empty() => cost_by_share(book, shares),
        (Some(book), _) if answered_is_unresolved(book, usage) => at_rates(book.dearest_rates(), u),
        (Some(book), _) if pricing.billing == Some(BillingMode::UsageBilled) => {
            at_rates(own_rates(pricing).or_else(|| book.dearest_rates()), u)
        }
        _ => footer::turn_cost(pricing, u),
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
    match turn_cost(&pricing, Some(book), usage, &u) {
        Some(c) => format!("  {tok} tok · ${c:.3}"),
        None => match unbilled_label(&pricing) {
            Some(label) => format!("  {tok} tok · {label}"),
            None => format!("  {tok} tok"),
        },
    }
}

/// The one-time notice when a fallback model answered. Its last sentence says
/// what the switch does to the cost: a subscription or local model adds
/// nothing per token, so "now priced for" would be untrue for it.
pub(super) fn fallback_notice(key: &str, answered: &str, from: &str, pricing: &Pricing) -> String {
    let cost = match unbilled_label(pricing) {
        Some(label) => format!("'{key}' is {label}: it adds nothing per token."),
        None => format!("Cost below is now priced for '{key}'."),
    };
    format!("⇄ answered by '{key}' ({answered}), not '{from}' — the runtime fell back. {cost}")
}

/// The fallback notice for `key` (the registry key that answered as
/// `answered`), or `None` when it is the configured model, when it is the
/// same model that answered the previous turn, or when the profile names no
/// model at all — with nothing configured there is nothing to fall back
/// *from*, so the first answer is not a fallback (#2625 review).
///
/// `last` records the model that answered the previous turn, whether or not
/// that turn announced anything; [`forget_answering_model`] clears it when a
/// turn's model cannot be resolved. Announces only on a CHANGE, so a long
/// session substituted once does not repeat it every turn. Shared by the TUI
/// and `--plain` so both say when the runtime fell back (#2609 review).
pub(super) fn fallback_notice_once(
    book: &PricingBook,
    key: &str,
    answered: &str,
    last: &mut Option<String>,
) -> Option<String> {
    let previous = last.replace(key.to_string());
    let configured = book.configured_key.as_deref()?;
    (key != configured && previous.as_deref() != Some(key))
        .then(|| fallback_notice(key, answered, configured, &book.pricing_for_key(key)))
}

/// A turn answered by a model the registry cannot resolve: it still breaks
/// the run, so the next resolvable fallback is announced again rather than
/// being taken for the one before it (sub → unknown → sub says so twice).
pub(super) fn forget_answering_model(last: &mut Option<String>) {
    *last = None;
}

/// `--plain`'s fallback notice for one finished turn, from its
/// `usage.model_ref`. `None` when the runtime did not say, the registry does
/// not know the model, or [`fallback_notice_once`] has nothing new to say.
pub(super) fn plain_fallback_notice(
    book: &PricingBook,
    usage: &serde_json::Value,
    last: &mut Option<String>,
) -> Option<String> {
    let answered = usage.get("model_ref").and_then(|v| v.as_str())?;
    let Some(key) = book.key_for_model(answered).map(str::to_string) else {
        forget_answering_model(last);
        return None;
    };
    fallback_notice_once(book, &key, answered, last)
}

/// USD for one turn priced share by share. `None` when no share is billed per
/// token (an all-subscription or all-local turn has no money figure). A share
/// whose model the registry cannot resolve or price is counted at the book's
/// dearest rates — the cap fails closed on what it cannot price.
fn cost_by_share(book: &PricingBook, shares: &[serde_json::Value]) -> Option<f64> {
    let mut total: Option<f64> = None;
    for share in shares {
        let tokens = |k: &str| share.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
        let u = UsageCounts {
            input: tokens("input_tokens"),
            output: tokens("output_tokens"),
        };
        if u.input == 0 && u.output == 0 {
            continue;
        }
        let rates = match book.key_for_share(share).map(|k| book.pricing_for_key(&k)) {
            Some(p) if unbilled_label(&p).is_some() => continue,
            Some(p) => own_rates(&p).or_else(|| book.dearest_rates()),
            None => book.dearest_rates(),
        };
        // Nothing in the registry carries a rate: there is no figure to give.
        let Some(c) = at_rates(rates, &u) else {
            continue;
        };
        *total.get_or_insert(0.0) += c;
    }
    total
}

#[cfg(test)]
#[path = "turn_price_tests.rs"]
mod tests;
