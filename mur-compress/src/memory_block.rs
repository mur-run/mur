//! Blocking-send UX and the one-time migration notice (P1 plan §9, §10).
//!
//! The copy lives here, beside the budget it reports, rather than in the TUI:
//! plan §9 pins these words in prose, and this crate builds in environments
//! where the chat pane's crate does not. The TUI decides WHERE to paint the
//! result; it never decides what it says.
//!
//! Every function is a pure `String` builder over an already-computed
//! [`RequiredUsageSummary`], so the overlay, the `/memories` meter, and the
//! runtime block cannot disagree about the numbers (plan invariant 5).
//!
//! Required means **injection, not compliance** (plan invariant 4) — the copy
//! here says "added to the context", never "guaranteed" or "obeyed".

use crate::memory_ux::RequiredUsageSummary;

/// `1234` → `1,234`.
///
/// Plan §9 writes budget figures as `4,320 / 3,500`; a four-digit token count
/// is easy to misread without the separator. Duplicated from the CLI's own
/// helper rather than shared, because `mur-compress` must not depend on
/// `mur-core` — and the format is three lines of arithmetic, not a policy.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The blocking overlay shown in the originating conversation when a send is
/// refused because the Required set does not fit (plan §9).
///
/// Four things this text must do, each of which the tests pin:
///
/// 1. Say plainly that **the message has not been sent** — a user who thinks
///    the turn is in flight will wait for a reply nobody asked for.
/// 2. Show `used / budget`, so "too big" is a number and not a mood.
/// 3. Give a reduction TARGET (`reduce by at least ~N`) while naming no
///    victim: the manage screen may sort by size, but nothing in MUR judges
///    which permanent instruction matters more.
/// 4. Offer no escape hatch. P1 has no skip-for-this-reply, continue-anyway,
///    inject-what-fits, or temporary override — those need turn-local state
///    and override lifecycle, which is P2. Offering one here would promise
///    behavior the injector will not honor.
///
/// It also states composer preservation explicitly: the typed message stays
/// put, the model was never called, and the user presses Enter again himself.
/// There is no automatic retry.
pub fn blocked_send_overlay(usage: &RequiredUsageSummary) -> String {
    let mut out = String::new();
    out.push_str("⛔ Permanent instructions don't fit — nothing was sent.\n\n");
    out.push_str("Your message has not been sent. The AI was not called.\n");
    out.push_str("It is still in your composer — press Enter again once there is room.\n\n");
    out.push_str(&format!(
        "Permanent instructions use {} of {} tokens.\n",
        thousands(usage.used_tokens),
        thousands(usage.budget_tokens)
    ));
    out.push_str(&format!(
        "Reduce by at least ~{} tokens to send again.\n\n",
        thousands(usage.reduce_by_tokens)
    ));
    // The action, not a recommendation: /memories lists each instruction with
    // its size and offers edit / remember-only-when-relevant / delete.
    out.push_str("/memories — manage permanent instructions (each one is listed with its size).\n");
    // Why there is no "send anyway": every permanent instruction is injected
    // or none is (invariant 2). A partial send would silently drop one of the
    // instructions the user explicitly asked to be present every turn.
    out.push_str("Every permanent instruction is added, or none is — so there is no partial send.");
    out
}

/// The overlay, but only when the set is actually over budget.
///
/// The send path calls this: `None` means "nothing to block, carry on". Having
/// one gate rather than an `if usage.over_budget` at each call site is what
/// stops a future caller from rendering a blocking overlay over a healthy set.
pub fn blocked_send_overlay_if_blocked(usage: &RequiredUsageSummary) -> Option<String> {
    usage.over_budget.then(|| blocked_send_overlay(usage))
}

/// The one-time, non-blocking notice announcing permanent instructions
/// (plan §10).
///
/// Deliberately **generic**. Migration moved every existing memory to
/// BestEffort — including ones whose text reads like a standing order — and
/// this notice must not undo that by suggestion. It recommends no specific
/// memory, names no candidate, and changes nothing: Required may only ever
/// arise from explicit user action (invariant 3).
///
/// That restraint is the whole design. "We noticed '永遠用中文' looks like a
/// permanent instruction — promote it?" would be a classifier with a human in
/// the loop, and P1 has no classifier at all.
pub fn migration_notice() -> String {
    let mut out = String::new();
    out.push_str("✨ New: permanent instructions.\n\n");
    out.push_str(
        "You can now mark an instruction to be added to the AI's context every turn, \
         instead of only when it seems relevant.\n\n",
    );
    // State the no-op plainly. A feature announcement that arrives beside a
    // list of existing memories reads as "something was done to them".
    out.push_str(
        "Nothing has changed: your existing memories are all still used only when relevant.\n\n",
    );
    out.push_str("/instruct <text> — add a permanent instruction\n");
    out.push_str("/memories — see both kinds side by side\n\n");
    out.push_str(
        "Being added every turn means the instruction is present in the context — \
         not that the model always follows it.",
    );
    out
}
