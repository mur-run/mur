//! Write-time behavior and `/memories` budget UX (P1 plan §6, §7, §9, §11).
//!
//! Plan §7 table — the contract these tests pin:
//!
//! | Operation | < 80% | 80–100% | > 100% |
//! |---|---|---|---|
//! | Add instruction | create | create + soft warning | reject |
//! | Make permanent | promote | promote + warning | reject |
//! | Edit Required | save | save + warning | warn + Save anyway |
//!
//! Edit is the ONLY operation that may deliberately create overflow state.
//!
//! These exercise the canonical API in `memory_budget` — there is deliberately
//! no second projection or decision implementation (plan invariant 5).

use mur_compress::memory_budget::{
    REQUIRED_BUDGET_TOKENS, RenderedRequiredMemory, RequiredBudgetProjection, WriteDecision,
    WriteOperation, canonical_memory_counter,
};
use mur_compress::memory_ux::usage_summary;

/// Exactly `tokens` tokens under the canonical counter.
///
/// "abc " is 4 bytes, so the bytes/4 heuristic and tiktoken agree at one
/// token per word and their max is the word count. Verified, not assumed:
/// `sized(n)` counts exactly `n` for n = 10, 100, 1000.
fn sized(tokens: usize) -> String {
    vec!["abc"; tokens].join(" ")
}

/// Token cost of `text` under the one canonical counter.
fn tokens(text: &str) -> isize {
    canonical_memory_counter().count(text) as isize
}

/// Decide a write of `incoming` against the existing `current` set.
fn decide(
    op: WriteOperation,
    current: &[RenderedRequiredMemory],
    incoming: &str,
) -> (WriteDecision, RequiredBudgetProjection) {
    let p = RequiredBudgetProjection::from_rendered(current, tokens(incoming));
    (p.decide(op), p)
}

/// The helper must really produce the size it claims, or every threshold test
/// below is meaningless.
#[test]
fn sized_helper_produces_exact_token_counts() {
    for n in [10usize, 100, 1000] {
        assert_eq!(tokens(&sized(n)), n as isize, "sized({n}) must count {n}");
    }
}

/// The projection reports current, delta, and projected together, against the
/// fixed budget — one arithmetic used by add, promote, and edit alike
/// (plan §6, invariant 5).
#[test]
fn projection_reports_current_delta_and_projected_against_fixed_budget() {
    let current = vec![RenderedRequiredMemory::new("m1", sized(100))];
    let p = RequiredBudgetProjection::from_rendered(&current, tokens(&sized(40)));

    assert_eq!(
        p.budget_tokens, REQUIRED_BUDGET_TOKENS,
        "projection must use the one fixed budget, not a local constant"
    );
    assert_eq!(p.current_tokens, 100);
    assert_eq!(p.delta_tokens, 40);
    assert_eq!(
        p.projected_tokens as isize,
        p.current_tokens as isize + p.delta_tokens,
        "projected must equal current + delta"
    );
}

/// Plan §7 row 1: an add that lands under 80% just creates, no warning.
#[test]
fn add_below_warning_threshold_is_allowed_silently() {
    let current = vec![RenderedRequiredMemory::new("m1", sized(100))];
    let (d, p) = decide(WriteOperation::Add, &current, &sized(50));
    assert_eq!(d, WriteDecision::Allow, "under 80% must not warn: {p:?}");
}

/// Plan §7 row 1, middle column: 80–100% still creates, but warns.
#[test]
fn add_inside_warning_band_is_allowed_with_a_warning() {
    // 2900 + 100 = 3000, which is 85.7% of 3500: warn, but still create.
    let current = vec![RenderedRequiredMemory::new("m1", sized(2900))];
    let (d, p) = decide(WriteOperation::Add, &current, &sized(100));
    assert_eq!(
        d,
        WriteDecision::AllowWithWarning,
        "80-100% must warn AND still create: {p:?}"
    );
    assert!(!p.is_over_budget(), "3000 of 3500 is not overflow: {p:?}");
}

/// Plan §7 row 1, right column, and acceptance test 7: over budget the add is
/// REJECTED outright — there is no "Add anyway" for creation.
#[test]
fn add_over_budget_is_rejected_with_no_override() {
    let current = vec![RenderedRequiredMemory::new("m1", sized(3200))];
    let (d, p) = decide(WriteOperation::Add, &current, &sized(500));
    assert_eq!(
        d,
        WriteDecision::Reject,
        "over budget must reject the add: {p:?}"
    );
}

/// Plan §7 row 2 and acceptance test 8: promotion over budget is rejected, so
/// the memory stays BestEffort.
#[test]
fn promote_over_budget_is_rejected() {
    let current = vec![RenderedRequiredMemory::new("m1", sized(3400))];
    let (d, _) = decide(WriteOperation::Promote, &current, &sized(300));
    assert_eq!(d, WriteDecision::Reject);
}

/// Plan §7 row 3 and acceptance test 9: edit is the ONE operation allowed to
/// create overflow deliberately — it warns but still permits "Save anyway",
/// because users merging or rewriting instructions must not be locked out.
#[test]
fn edit_over_budget_warns_but_allows_save_anyway() {
    let current = vec![RenderedRequiredMemory::new("m1", sized(3400))];
    let (d, p) = decide(WriteOperation::Edit, &current, &sized(300));
    assert_eq!(
        d,
        WriteDecision::AllowWithWarning,
        "edit must stay possible in overflow: {p:?}"
    );
    assert!(
        p.is_over_budget(),
        "this edit really does enter overflow: {p:?}"
    );
}

/// The boundary: a set that exactly fills the reservation still fits, so
/// write-time and runtime cannot disagree at the edge.
#[test]
fn exactly_filling_the_budget_is_not_overflow() {
    let current = vec![RenderedRequiredMemory::new("m1", sized(3000))];
    let (d, p) = decide(WriteOperation::Add, &current, &sized(500));
    assert_eq!(p.projected_tokens, REQUIRED_BUDGET_TOKENS);
    assert!(!p.is_over_budget(), "== budget must fit: {p:?}");
    assert_eq!(
        d,
        WriteDecision::AllowWithWarning,
        "at 100% it warns, but does not reject: {p:?}"
    );
}

/// Plan §9: the overflow screen must tell the user how much to cut. It is a
/// reduction target, never a judgement about which instruction matters.
#[test]
fn overflow_reports_how_many_tokens_to_reduce_by() {
    let current = vec![RenderedRequiredMemory::new("m1", sized(4320))];
    let u = usage_summary(&current);

    assert!(u.over_budget, "4320 > 3500 must read as over budget: {u:?}");
    assert_eq!(
        u.reduce_by_tokens,
        4320 - REQUIRED_BUDGET_TOKENS,
        "must report the exact shortfall"
    );
    assert_eq!(u.used_tokens, 4320);
    assert_eq!(u.budget_tokens, REQUIRED_BUDGET_TOKENS);
}

/// Within budget there is nothing to reduce, so the meter offers no target.
#[test]
fn within_budget_has_no_reduction_target() {
    let current = vec![RenderedRequiredMemory::new("m1", sized(2800))];
    let u = usage_summary(&current);
    assert!(!u.over_budget);
    assert_eq!(u.reduce_by_tokens, 0);
}

/// Plan §9: the recovery screen lists per-item sizes so the user can choose
/// what to cut. Input order is preserved — sorting by size is a presentation
/// choice the caller may make, and the summary must never imply importance
/// (invariant 1).
#[test]
fn usage_summary_reports_per_item_sizes_in_input_order() {
    let current = vec![
        RenderedRequiredMemory::new("first", sized(30)),
        RenderedRequiredMemory::new("second", sized(10)),
        RenderedRequiredMemory::new("third", sized(20)),
    ];
    let u = usage_summary(&current);
    let ids: Vec<&str> = u.per_item.iter().map(|i| i.memory_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["first", "second", "third"],
        "usage must not reorder the Required set"
    );
    assert_eq!(u.per_item[0].rendered_tokens, 30);
    assert_eq!(u.per_item[1].rendered_tokens, 10);
    assert_eq!(u.per_item[2].rendered_tokens, 20);
}

/// Plan §5 and acceptance test 16: the count warning is soft. At the threshold
/// it warns, but it must NEVER block — only the token budget governs creation.
#[test]
fn count_warning_is_advisory_and_never_blocks() {
    let many: Vec<RenderedRequiredMemory> = (0..50)
        .map(|i| RenderedRequiredMemory::new(format!("m{i}"), sized(1)))
        .collect();
    let u = usage_summary(&many);
    assert!(u.count_warning, "at the threshold it must warn");
    assert!(
        !u.over_budget,
        "50 tiny memories are nowhere near the token budget, so nothing blocks"
    );

    // And a write in that state is still allowed.
    let (d, _) = decide(WriteOperation::Add, &many, &sized(1));
    assert_eq!(d, WriteDecision::Allow, "a count warning must not block");
}

/// Plan invariant 5 and acceptance test 14: the write-time projection and the
/// usage meter must agree, because they share one counter and one budget.
#[test]
fn projection_and_usage_meter_agree_on_token_counts() {
    let current = vec![
        RenderedRequiredMemory::new("m1", sized(500)),
        RenderedRequiredMemory::new("m2", sized(250)),
    ];
    let u = usage_summary(&current);
    let p = RequiredBudgetProjection::from_rendered(&current, 0);

    assert_eq!(
        u.used_tokens, p.current_tokens,
        "one tokenizer, one budget: the meter and the projection must match"
    );
    assert_eq!(u.budget_tokens, p.budget_tokens);
    assert_eq!(
        u.used_tokens,
        u.per_item.iter().map(|i| i.rendered_tokens).sum::<usize>(),
        "the total must be the sum of the parts it displays"
    );
}

/// Acceptance test 10: demoting a Required memory is a negative delta that
/// clears the blocking state. Nothing auto-demotes — this models the user
/// explicitly choosing one (plan §7).
#[test]
fn demotion_is_a_negative_delta_that_clears_overflow() {
    let current = vec![
        RenderedRequiredMemory::new("big", sized(1000)),
        RenderedRequiredMemory::new("rest", sized(3320)),
    ];
    assert!(usage_summary(&current).over_budget, "starts blocked");

    let after = RequiredBudgetProjection::from_rendered(&current, -tokens(&sized(1000)));
    assert_eq!(after.projected_tokens, 3320);
    assert!(
        !after.is_over_budget(),
        "demoting the 1000-token memory must clear the block: {after:?}"
    );
}
