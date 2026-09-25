//! Acceptance tests for the Required-memory budget layer (P1 plan §5, §6, §8).
//!
//! Plan invariant 2: "No silent omission: either every Required memory is
//! injected, or inference is blocked with `RequiredBudgetExceeded`."
//!
//! These tests exercise the budget layer at its public seam. The layer is
//! deliberately decoupled from the memory schema: it takes already-rendered
//! strings, so it can be reused by the injector, by write-time projection,
//! and by the `/memories` usage meter without any of them depending on each
//! other.

use mur_compress::memory_budget::{
    REQUIRED_BUDGET_TOKENS, RenderedRequiredMemory, RequiredBudgetCheck, canonical_memory_counter,
    check_required_budget,
};

/// Build a rendered Required memory measuring at most `target_tokens` tokens
/// under the canonical counter.
///
/// Sizing is done by MEASUREMENT, not by assuming one word == one token.
/// `"word "` is 5 bytes, so the bytes/4 heuristic charges ceil(5n/4) — 1.25
/// tokens per word — and the canonical counter takes the max of the two
/// counters. Repeating the unit `target_tokens` times therefore overshoots the
/// target by ~25%, which is what made the 19-of-20 precondition below
/// unsatisfiable. We scale to an estimate, then walk down to the largest size
/// that genuinely fits.
fn rendered(id: &str, target_tokens: usize) -> RenderedRequiredMemory {
    let counter = canonical_memory_counter();
    let unit = "word ";

    // Scale from one measurement: measured(n) is linear in n for a repeated
    // unit, so target * n / measured(n) lands within a word or two.
    let probe_units = target_tokens.max(1);
    let probe_tokens = counter.count(&unit.repeat(probe_units)).max(1);
    let mut units = (probe_units * target_tokens / probe_tokens).max(1);

    // Walk up while there is room, then down until it fits: the result is the
    // largest word count whose MEASURED size is <= target_tokens.
    while counter.count(&unit.repeat(units + 1)) <= target_tokens {
        units += 1;
    }
    while units > 1 && counter.count(&unit.repeat(units)) > target_tokens {
        units -= 1;
    }

    RenderedRequiredMemory::new(id, unit.repeat(units))
}

/// T3 / acceptance test 5 — No partial fallback.
///
/// 19 of 20 Required memories fit the budget. The system must block, and must
/// not quietly inject the 19 that fit.
#[test]
fn overflow_blocks_instead_of_injecting_what_fits() {
    let counter = canonical_memory_counter();

    // 19 memories that comfortably fit the fixed budget...
    let per_memory_tokens = REQUIRED_BUDGET_TOKENS / 20;
    let mut memories: Vec<RenderedRequiredMemory> = (0..19)
        .map(|i| rendered(&format!("mem-{i}"), per_memory_tokens))
        .collect();

    let fitting_total: usize = memories
        .iter()
        .map(|m| counter.count(m.rendered()))
        .sum::<usize>();
    assert!(
        fitting_total <= REQUIRED_BUDGET_TOKENS,
        "precondition: the first 19 must fit ({fitting_total} <= {REQUIRED_BUDGET_TOKENS})"
    );

    // ...plus one more that pushes the set over the fixed budget.
    memories.push(rendered("mem-19", REQUIRED_BUDGET_TOKENS));

    match check_required_budget(&memories) {
        RequiredBudgetCheck::Exceeded {
            required,
            required_tokens,
            required_budget_tokens,
        } => {
            assert!(
                required_tokens > required_budget_tokens,
                "overflow must report tokens over budget"
            );
            assert_eq!(required_budget_tokens, REQUIRED_BUDGET_TOKENS);
            // The overflow report is diagnostic: it lists EVERY Required
            // memory with its size so the user can choose what to cut. It is
            // not an injection set.
            assert_eq!(
                required.len(),
                20,
                "overflow must report all Required memories, not just the ones that fit"
            );
        }
        RequiredBudgetCheck::WithinBudget { .. } => {
            panic!("19-of-20 must block, never inject the subset that fits");
        }
    }
}

/// Acceptance test 3 — Fits budget: every Required memory is injected.
#[test]
fn within_budget_yields_every_required_memory() {
    let memories: Vec<RenderedRequiredMemory> =
        (0..10).map(|i| rendered(&format!("mem-{i}"), 20)).collect();

    match check_required_budget(&memories) {
        RequiredBudgetCheck::WithinBudget {
            usage,
            total_tokens,
            budget_tokens,
        } => {
            assert_eq!(usage.len(), 10, "all Required memories are injected");
            assert_eq!(budget_tokens, REQUIRED_BUDGET_TOKENS);
            assert!(total_tokens <= budget_tokens);
            assert_eq!(
                total_tokens,
                usage.iter().map(|u| u.rendered_tokens).sum::<usize>(),
                "total must be the sum of the per-memory counts shown to the user"
            );
            let ids: Vec<&str> = usage.iter().map(|u| u.memory_id.as_str()).collect();
            assert_eq!(
                ids,
                (0..10).map(|i| format!("mem-{i}")).collect::<Vec<_>>(),
                "Required preserves input order: no ranking, no sort by size"
            );
        }
        RequiredBudgetCheck::Exceeded { .. } => panic!("200 tokens must fit in the fixed budget"),
    }
}

/// An empty Required set is within budget, not an error.
#[test]
fn empty_required_set_is_within_budget() {
    match check_required_budget(&[]) {
        RequiredBudgetCheck::WithinBudget {
            usage,
            total_tokens,
            ..
        } => {
            assert!(usage.is_empty());
            assert_eq!(total_tokens, 0);
        }
        RequiredBudgetCheck::Exceeded { .. } => panic!("no Required memories cannot overflow"),
    }
}

/// Plan §5: "P1 picks the most conservative tokenizer as canonical."
///
/// The heuristic (bytes/4) undercounts CJK badly relative to a real
/// tokenizer, so the canonical counter must take the max of the two.
#[test]
fn canonical_counter_is_conservative_for_cjk() {
    use mur_compress::tokenizer::{HeuristicCounter, TiktokenCounter, TokenCounter};

    let canonical = canonical_memory_counter();
    let heuristic = HeuristicCounter;
    let tiktoken = TiktokenCounter::new().expect("cl100k_base loads");

    let text = "永遠用中文回答我，不要使用英文。";
    let canonical_count = canonical.count(text);

    assert!(
        canonical_count >= heuristic.count(text),
        "canonical must never undercount relative to the heuristic"
    );
    assert!(
        canonical_count >= tiktoken.count(text),
        "canonical must never undercount relative to tiktoken"
    );
    assert_eq!(
        canonical_count,
        heuristic.count(text).max(tiktoken.count(text)),
        "canonical is exactly the conservative max"
    );
}
