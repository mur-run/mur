//! Blocking-send UX and the one-time migration notice (P1 plan §9, §10).
//!
//! These live in `mur-compress` rather than next to the TUI on purpose: this
//! crate compiles and tests in environments where `mur-core` cannot (it pulls
//! `protoc` through `lance-encoding`), and the exact copy is a contract the
//! plan pins in prose. Testing the words here means the blocking path has
//! real coverage even when the pane that renders it does not build.
//!
//! The functions are pure `String` builders: the TUI decides where to paint
//! them, never what they say.

use mur_compress::memory_block::{blocked_send_overlay, migration_notice};
use mur_compress::memory_budget::{RenderedRequiredMemory, REQUIRED_BUDGET_TOKENS};
use mur_compress::memory_ux::usage_summary;

/// Exactly `tokens` tokens under the canonical counter ("abc " = 1 token).
fn sized(tokens: usize) -> String {
    vec!["abc"; tokens].join(" ")
}

/// A Required set that overflows the fixed budget by a known margin.
fn overflowing_set() -> Vec<RenderedRequiredMemory> {
    // 4 × 1,000 = 4,000 against a 3,500 budget → over by ~500.
    (0..4)
        .map(|i| RenderedRequiredMemory::new(format!("note-{i}"), sized(1000)))
        .collect()
}

/// Plan §9: the overlay MUST state that the message was not sent. This is the
/// single most important line — a user who believes the turn went through will
/// wait forever for a reply that was never requested.
#[test]
fn overlay_says_the_message_was_not_sent() {
    let usage = usage_summary(&overflowing_set());
    let text = blocked_send_overlay(&usage);
    assert!(
        text.contains("Your message has not been sent."),
        "overlay must say the message was not sent, verbatim:\n{text}"
    );
}

/// Plan §9: usage is shown as `used / budget`, so the user can see how far
/// over they are rather than being told only that something is wrong.
#[test]
fn overlay_shows_usage_against_the_budget() {
    let usage = usage_summary(&overflowing_set());
    let text = blocked_send_overlay(&usage);
    assert!(
        text.contains("4,000") && text.contains("3,500"),
        "overlay must show used / budget with thousands separators:\n{text}"
    );
    assert_eq!(
        usage.budget_tokens, REQUIRED_BUDGET_TOKENS,
        "the overlay must measure against the one fixed budget"
    );
}

/// Plan §9: "reduce by at least ~N tokens" — a reduction TARGET. It must never
/// name a specific instruction to drop, because nothing in MUR may judge which
/// permanent instruction matters more.
#[test]
fn overlay_gives_a_reduction_target_and_names_no_victim() {
    let usage = usage_summary(&overflowing_set());
    let text = blocked_send_overlay(&usage);
    assert!(
        text.contains("500"),
        "overlay must state how much to cut:\n{text}"
    );
    // The per-item sizes are offered by the manage screen, not chosen here.
    for suspect in ["you should delete", "recommend", "least important", "drop note-"] {
        assert!(
            !text.to_lowercase().contains(suspect),
            "overlay must not judge importance ({suspect:?}):\n{text}"
        );
    }
}

/// Plan §9: a **Manage permanent instructions** action, and the return path.
#[test]
fn overlay_offers_the_manage_action() {
    let usage = usage_summary(&overflowing_set());
    let text = blocked_send_overlay(&usage);
    assert!(
        text.contains("/memories"),
        "overlay must route to the manage screen:\n{text}"
    );
}

/// Plan §9: "No escape hatch in P1" — no skip-for-this-reply, no
/// continue-anyway, no inject-what-fits, no temporary override. If any of these
/// words appear the UI is promising something the injector will not honor.
#[test]
fn overlay_offers_no_escape_hatch() {
    let usage = usage_summary(&overflowing_set());
    let text = blocked_send_overlay(&usage).to_lowercase();
    for hatch in [
        "send anyway",
        "continue anyway",
        "skip for this",
        "inject what fits",
        "temporarily",
        "override",
        "ignore for now",
    ] {
        assert!(
            !text.contains(hatch),
            "P1 forbids an escape hatch, found {hatch:?}:\n{text}"
        );
    }
}

/// Plan §9: composer preservation must be stated, so the user knows their
/// typed message is still there and that they press Send again themselves
/// (there is no automatic retry).
#[test]
fn overlay_promises_the_message_is_kept_and_not_auto_retried() {
    let usage = usage_summary(&overflowing_set());
    let text = blocked_send_overlay(&usage);
    let lower = text.to_lowercase();
    assert!(
        lower.contains("still in your composer") || lower.contains("kept"),
        "overlay must say the typed message is preserved:\n{text}"
    );
    assert!(
        lower.contains("press enter again") || lower.contains("send it again"),
        "overlay must say the user re-sends manually — no automatic retry:\n{text}"
    );
}

/// Required means injection, not compliance (invariant 4). The overlay is a
/// natural place for that wording to rot into "guaranteed", which §4 bans.
#[test]
fn overlay_avoids_banned_vocabulary() {
    let usage = usage_summary(&overflowing_set());
    let text = blocked_send_overlay(&usage).to_lowercase();
    for banned in ["guaranteed", "hard memory", "always memory"] {
        assert!(
            !text.contains(banned),
            "§4 bans {banned:?} as injection vocabulary:\n{text}"
        );
    }
}

/// Plan §10: the migration notice is GENERIC. It must recommend no specific
/// memory and must not claim to have changed or created anything — migration
/// creates zero Required memories (invariant 3).
#[test]
fn migration_notice_is_generic_and_promises_no_changes() {
    let text = migration_notice();
    let lower = text.to_lowercase();
    assert!(
        lower.contains("permanent instruction"),
        "the notice exists to announce the feature:\n{text}"
    );
    assert!(
        lower.contains("nothing has changed") || lower.contains("no memories were changed"),
        "the notice must state it modified nothing:\n{text}"
    );
    assert!(
        lower.contains("/instruct"),
        "the notice must say how to opt in explicitly:\n{text}"
    );
}

/// The notice must not nominate a candidate — "we noticed '永遠用中文' looks
/// permanent" is exactly the automatic reclassification invariant 3 forbids.
#[test]
fn migration_notice_nominates_no_candidate() {
    let lower = migration_notice().to_lowercase();
    for suspect in [
        "we noticed",
        "looks like",
        "we found",
        "suggest",
        "recommend",
        "converted",
        "promoted",
    ] {
        assert!(
            !lower.contains(suspect),
            "the notice must nominate nothing ({suspect:?}): {lower}"
        );
    }
}

/// Within budget there is nothing to block, so there is no overlay to show.
/// A "blocked" overlay on a healthy set would be a phantom error.
#[test]
fn no_overlay_when_within_budget() {
    let ok = vec![RenderedRequiredMemory::new("n", sized(100))];
    let usage = usage_summary(&ok);
    assert!(!usage.over_budget, "100 tokens must fit the budget");
    assert!(
        mur_compress::memory_block::blocked_send_overlay_if_blocked(&usage).is_none(),
        "a set within budget must produce no blocking overlay"
    );
}

/// And over budget it does produce one, with the same copy as the direct call.
#[test]
fn overlay_appears_when_over_budget() {
    let usage = usage_summary(&overflowing_set());
    assert_eq!(
        mur_compress::memory_block::blocked_send_overlay_if_blocked(&usage).as_deref(),
        Some(blocked_send_overlay(&usage).as_str()),
        "the gate and the direct builder must not drift apart"
    );
}
