//! `/memories` budget UX (P1 plan §9 "Blocking UX", §11 "`/memories`
//! layout").
//!
//! The write-time projection and the §7 decision table live in
//! [`crate::memory_budget`] — this module deliberately does NOT restate them.
//! Plan invariant 5 is "one rendered representation, one tokenizer, one budget
//! profile across write-time projection, UI display, and runtime validation",
//! so the usage meter here is built ON that projection rather than beside it.
//!
//! Like `memory_budget`, this is decoupled from the memory schema: it takes
//! already-rendered strings.
//!
//! Required means **injection, not compliance** (plan invariant 4).

use crate::memory_budget::{
    REQUIRED_BUDGET_TOKENS, RenderedRequiredMemory, RequiredMemoryUsage, canonical_memory_counter,
    required_count_warning,
};

/// The `/memories` usage meter and the overflow recovery screen (plan §9,
/// §11).
///
/// Reports size only. It deliberately carries no notion of importance: the
/// manage screen may sort or highlight by size, but nothing in MUR may judge
/// which permanent instruction matters more (plan §9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredUsageSummary {
    /// Tokens the Required set uses right now.
    pub used_tokens: usize,
    /// The fixed reservation.
    pub budget_tokens: usize,
    /// Per-item sizes, in input order — never sorted here (invariant 1).
    pub per_item: Vec<RequiredMemoryUsage>,
    /// True when the set exceeds the budget, i.e. sends are blocked.
    pub over_budget: bool,
    /// How much to cut to clear the block; 0 when within budget.
    ///
    /// Plan §9's "reduce by at least ~N tokens" — a reduction target, never a
    /// recommendation about which instruction to drop.
    pub reduce_by_tokens: usize,
    /// Soft advisory only — this NEVER blocks a write (plan §5).
    pub count_warning: bool,
}

/// Summarize the current Required set for display.
///
/// Uses [`canonical_memory_counter`] and [`REQUIRED_BUDGET_TOKENS`] — the same
/// counter and budget as the write-time projection and runtime validation — so
/// the meter, the warning, and the block always agree (plan invariant 5,
/// acceptance test 14).
pub fn usage_summary(current: &[RenderedRequiredMemory]) -> RequiredUsageSummary {
    let counter = canonical_memory_counter();
    let per_item: Vec<RequiredMemoryUsage> = current
        .iter()
        .map(|m| RequiredMemoryUsage {
            memory_id: m.memory_id().to_string(),
            rendered_tokens: counter.count(m.rendered()),
        })
        .collect();

    let used_tokens: usize = per_item.iter().map(|i| i.rendered_tokens).sum();

    RequiredUsageSummary {
        used_tokens,
        budget_tokens: REQUIRED_BUDGET_TOKENS,
        per_item,
        // Strictly greater than, matching `check_required_budget` and
        // `is_over_budget`: a set that exactly fills the reservation fits, so
        // the meter never claims a block the runtime would not make.
        over_budget: used_tokens > REQUIRED_BUDGET_TOKENS,
        reduce_by_tokens: used_tokens.saturating_sub(REQUIRED_BUDGET_TOKENS),
        count_warning: required_count_warning(current.len()),
    }
}
