//! Acceptance tests for write-time token accounting (P1 plan §5 "Guardrails",
//! §6 "Token accounting", §7 "Write-time behavior").
//!
//! Plan invariant 5: "One rendered representation, one tokenizer, one budget
//! profile across write-time projection, UI display, and runtime validation."
//!
//! These tests cover the pieces `check_required_budget` does not: the single
//! rendered representation (`render_for_injection`), the shared projection
//! used by add / promote / edit, and the per-memory + count guardrails.

use mur_compress::memory_budget::{
    MAX_REQUIRED_MEMORY_CHARS, REQUIRED_BUDGET_TOKENS, REQUIRED_MEMORY_COUNT_WARNING,
    RenderedRequiredMemory, RequiredBudgetProjection, WriteDecision, WriteOperation,
    canonical_memory_counter, exceeds_per_memory_char_limit, project_required_budget,
    render_for_injection, required_count_warning,
};

/// Plan §6: count `render_for_injection(memory)` — "including wrapper/prefix/
/// separator overhead — never bare `content`".
///
/// The expected value is a known-good literal taken from the production note
/// builder in `mur-agent-runtime/src/skills/injector.rs`, which renders each
/// memory as `- {name}: {body}`. If that format ever changes, this test must
/// change with it: it is the contract that write-time projection and runtime
/// injection measure the SAME string.
#[test]
fn render_for_injection_matches_the_production_note_line() {
    assert_eq!(
        render_for_injection("reply-in-zh-tw", "永遠用中文回覆我"),
        "- reply-in-zh-tw: 永遠用中文回覆我"
    );
}

/// The body is trimmed, exactly as the production builder does (`body.trim()`),
/// so trailing whitespace cannot inflate the measured cost.
#[test]
fn render_for_injection_trims_the_body() {
    assert_eq!(
        render_for_injection("no-force-push", "  never force-push main  \n"),
        "- no-force-push: never force-push main"
    );
}

/// Rendering overhead is real and must be charged: the rendered form costs
/// strictly more tokens than the bare content it wraps.
#[test]
fn rendered_form_costs_more_than_bare_content() {
    let counter = canonical_memory_counter();
    let body = "永遠用中文回覆我";

    let bare = counter.count(body);
    let rendered = counter.count(&render_for_injection("reply-in-zh-tw", body));

    assert!(
        rendered > bare,
        "wrapper/prefix overhead must be counted: {rendered} > {bare}"
    );
}

/// Acceptance test 6 — Add warning: projected 2,900 / 3,500 (82.9%) → warn,
/// creation still allowed.
#[test]
fn add_within_warning_band_warns_but_allows() {
    let projection = project_required_budget(2_400, 500);

    assert_eq!(projection.current_tokens, 2_400);
    assert_eq!(projection.delta_tokens, 500);
    assert_eq!(projection.projected_tokens, 2_900);
    assert_eq!(projection.budget_tokens, REQUIRED_BUDGET_TOKENS);
    assert_eq!(
        projection.decide(WriteOperation::Add),
        WriteDecision::AllowWithWarning,
        "82.9% of budget must warn, not reject"
    );
}

/// Below the warning band an add is silent — no warning noise.
#[test]
fn add_well_under_budget_is_allowed_silently() {
    let projection = project_required_budget(500, 100);

    assert_eq!(projection.projected_tokens, 600);
    assert_eq!(projection.decide(WriteOperation::Add), WriteDecision::Allow);
}

/// Acceptance test 7 — Add overflow: 3,200 + 500 / 3,500 → rejected, and
/// there is deliberately no "Add anyway" (plan §7).
#[test]
fn add_over_budget_is_rejected() {
    let projection = project_required_budget(3_200, 500);

    assert_eq!(projection.projected_tokens, 3_700);
    assert!(projection.is_over_budget());
    assert_eq!(
        projection.decide(WriteOperation::Add),
        WriteDecision::Reject,
        "add over budget must be rejected with no escape hatch"
    );
}

/// Acceptance test 8 — Promotion overflow is rejected; the memory stays
/// BestEffort.
#[test]
fn promote_over_budget_is_rejected() {
    let projection = project_required_budget(3_200, 500);

    assert_eq!(
        projection.decide(WriteOperation::Promote),
        WriteDecision::Reject
    );
}

/// Acceptance test 9 — Edit overflow: warns, but "Save anyway" is allowed.
///
/// Edit is the ONLY operation that may deliberately create overflow state:
/// users merging or rewriting instructions must not be locked out of editing.
#[test]
fn edit_over_budget_warns_but_is_allowed() {
    let projection = project_required_budget(3_200, 500);

    assert!(projection.is_over_budget());
    assert_eq!(
        projection.decide(WriteOperation::Edit),
        WriteDecision::AllowWithWarning,
        "edit must stay possible even into overflow"
    );
}

/// Acceptance test 10 — Demotion recovery: removing a 1,000-token Required
/// memory takes 4,320 down to 3,320, and the blocking state clears.
#[test]
fn demotion_reduces_projection_and_clears_overflow() {
    let projection = project_required_budget(4_320, -1_000);

    assert_eq!(projection.delta_tokens, -1_000);
    assert_eq!(projection.projected_tokens, 3_320);
    assert!(
        !projection.is_over_budget(),
        "3,320 / 3,500 must clear the blocking state"
    );
}

/// A delta larger than the current usage saturates at zero rather than
/// underflowing — `projected_tokens` is a count, never negative.
#[test]
fn oversized_negative_delta_saturates_at_zero() {
    let projection = project_required_budget(200, -1_000);

    assert_eq!(projection.projected_tokens, 0);
    assert!(!projection.is_over_budget());
}

/// Exactly at the budget is within it: the check is `>` budget, not `>=`, so
/// a set that precisely fills the reservation is still injectable. This
/// matches `check_required_budget`, keeping write-time and runtime in
/// agreement (plan invariant 5).
#[test]
fn exactly_at_budget_is_not_overflow() {
    let projection = project_required_budget(REQUIRED_BUDGET_TOKENS, 0);

    assert!(!projection.is_over_budget());
    assert_eq!(
        projection.decide(WriteOperation::Add),
        WriteDecision::AllowWithWarning,
        "100% is inside the warning band but not rejected"
    );
}

/// Plan §6: the projection must be computable from the rendered Required set,
/// so the `/memories` usage meter and the projection cannot disagree.
#[test]
fn projection_current_tokens_match_the_rendered_set() {
    let counter = canonical_memory_counter();
    let set = vec![
        RenderedRequiredMemory::new("a", render_for_injection("a", "always reply in Chinese")),
        RenderedRequiredMemory::new("b", render_for_injection("b", "never force-push main")),
    ];

    let expected: usize = set.iter().map(|m| counter.count(m.rendered())).sum();
    let projection = RequiredBudgetProjection::from_rendered(&set, 0);

    assert_eq!(projection.current_tokens, expected);
    assert_eq!(projection.projected_tokens, expected);
}

/// Plan §5: `MAX_REQUIRED_MEMORY_CHARS` is a per-memory validation error,
/// independent of the budget — one enormous instruction is rejected on its
/// own terms.
#[test]
fn per_memory_char_limit_rejects_only_oversized_content() {
    let ok = "x".repeat(MAX_REQUIRED_MEMORY_CHARS);
    let too_big = "x".repeat(MAX_REQUIRED_MEMORY_CHARS + 1);

    assert!(
        !exceeds_per_memory_char_limit(&ok),
        "at the limit is allowed"
    );
    assert!(
        exceeds_per_memory_char_limit(&too_big),
        "over the limit is rejected"
    );
}

/// The char limit counts characters, not bytes: a CJK instruction well under
/// the limit in characters must not be rejected for being 3 bytes per char.
#[test]
fn per_memory_char_limit_counts_chars_not_bytes() {
    let cjk = "永".repeat(MAX_REQUIRED_MEMORY_CHARS);

    assert!(
        cjk.len() > MAX_REQUIRED_MEMORY_CHARS,
        "precondition: bytes exceed the limit"
    );
    assert!(
        !exceeds_per_memory_char_limit(&cjk),
        "char limit must not be a byte limit"
    );
}

/// Plan §5: the count warning is soft and "never blocks" — it reports, and
/// creation remains governed by tokens (acceptance test 16).
#[test]
fn count_warning_is_soft_and_threshold_based() {
    assert!(!required_count_warning(REQUIRED_MEMORY_COUNT_WARNING - 1));
    assert!(
        required_count_warning(REQUIRED_MEMORY_COUNT_WARNING),
        "at the threshold, warn"
    );
    assert!(required_count_warning(REQUIRED_MEMORY_COUNT_WARNING + 10));

    // Soft means soft: hitting the count threshold does not change the
    // token-based decision for an otherwise-fine add.
    assert_eq!(
        project_required_budget(500, 100).decide(WriteOperation::Add),
        WriteDecision::Allow
    );
}

/// Acceptance test 14 — Token consistency: the projection's arithmetic and
/// the runtime budget check agree on the same rendered set.
#[test]
fn write_projection_and_runtime_check_agree() {
    use mur_compress::memory_budget::{RequiredBudgetCheck, check_required_budget};

    let set: Vec<RenderedRequiredMemory> = (0..5)
        .map(|i| {
            RenderedRequiredMemory::new(
                format!("mem-{i}"),
                render_for_injection(&format!("mem-{i}"), "always reply in Chinese"),
            )
        })
        .collect();

    let projection = RequiredBudgetProjection::from_rendered(&set, 0);

    match check_required_budget(&set) {
        RequiredBudgetCheck::WithinBudget { total_tokens, .. } => {
            assert_eq!(
                projection.current_tokens, total_tokens,
                "write-time projection and runtime validation must agree exactly"
            );
        }
        RequiredBudgetCheck::Exceeded { .. } => panic!("small set must fit"),
    }
}
