//! Required-memory budget layer (P1 plan §5 "Guardrails", §6 "Token
//! accounting", §8 "Injector pipeline").
//!
//! BUILD NOTE: if `cargo` fails here with `cc: can't exec '.../XcodeDefault
//! .xctoolchain/usr/bin/clang' (Operation not permitted)`, the Xcode
//! toolchain is blocked but the Command Line Tools one is not. Build with:
//!
//! ```text
//! export DEVELOPER_DIR=/Library/Developer/CommandLineTools
//! export CC=$DEVELOPER_DIR/usr/bin/clang
//! export CXX=$DEVELOPER_DIR/usr/bin/clang++
//! export SDKROOT=$DEVELOPER_DIR/SDKs/MacOSX.sdk
//! ```
//!
//! `tree-sitter` then compiles and the whole crate tests normally; an
//! isolated `#[path]` harness is not needed.
//!
//! This module is deliberately decoupled from the memory schema: every
//! function takes an already-rendered string. The injector, the write-time
//! projection, and the `/memories` usage meter therefore share one tokenizer
//! and one budget (plan invariant 5) without depending on each other.
//!
//! Required means **injection, not compliance** (plan invariant 4).

use crate::tokenizer::{HeuristicCounter, TiktokenCounter, TokenCounter};

/// Fixed token reservation for the whole Required set.
///
/// Derivation (plan §5, recorded here so it does not become an unsourced
/// magic number): 10–20 typical permanent instructions × 100–300 tokens each
/// ≈ 1,000–3,000 tokens, plus headroom for wrapper/prefix/separator overhead
/// and for the conservative CJK counting below → 3,500.
///
/// This is a *fixed reservation*, never "whatever context is left". A
/// permanent instruction must not work on turn 1 and silently fail on turn
/// 50. BestEffort yields to Required, never the reverse. Per-model budget
/// profiles are P2; P1 uses the most conservative tokenizer as canonical.
pub const REQUIRED_BUDGET_TOKENS: usize = 3500;

/// Per-memory character limit (plan §5). A single instruction longer than this
/// is a *validation error on that memory*, independent of how much budget is
/// free — it is rejected on its own terms, not by competition.
///
/// Characters, not bytes: a CJK instruction is 3 bytes per character, and a
/// byte limit would silently give Chinese users a third of the allowance.
pub const MAX_REQUIRED_MEMORY_CHARS: usize = 2000;

/// Soft count threshold (plan §5). At or above this many Required memories the
/// UI warns, but it "never blocks" — creation stays governed by tokens.
pub const REQUIRED_MEMORY_COUNT_WARNING: usize = 50;

/// Fraction of the budget above which write operations warn (plan §7's
/// 80–100% band).
const WARNING_RATIO: f64 = 0.8;

/// Render a memory exactly as it appears in the injected prompt.
///
/// This is the ONE rendered representation (plan invariant 5): write-time
/// projection, the `/memories` usage meter, and runtime validation all count
/// this string, so they cannot disagree.
///
/// The format mirrors the production note builder in
/// `mur-agent-runtime/src/skills/injector.rs` (`- {name}: {body}`, trimmed),
/// so the tokens counted here are the tokens actually spent. Changing the
/// injector's format without changing this function would reintroduce the
/// class of bug where a memory is budgeted as one size and injected as
/// another.
pub fn render_for_injection(name: &str, body: &str) -> String {
    format!("- {}: {}", name, body.trim())
}

/// Whether `content` violates the per-memory character limit (plan §5).
pub fn exceeds_per_memory_char_limit(content: &str) -> bool {
    content.chars().count() > MAX_REQUIRED_MEMORY_CHARS
}

/// Whether the Required-memory count warrants the soft warning (plan §5).
///
/// Advisory only: callers must never turn this into a block.
pub fn required_count_warning(required_count: usize) -> bool {
    required_count >= REQUIRED_MEMORY_COUNT_WARNING
}

/// The write operation a projection is being judged for (plan §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOperation {
    /// Add instruction — rejected over budget, with no "Add anyway".
    Add,
    /// Make permanent — rejected over budget; the memory stays BestEffort.
    Promote,
    /// Edit a Required memory — may deliberately enter overflow.
    Edit,
}

/// What the UI must do with a proposed write (plan §7's three columns).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteDecision {
    /// Under the warning band: proceed silently.
    Allow,
    /// In the 80–100% band, or an edit deliberately entering overflow.
    AllowWithWarning,
    /// Over budget: refuse. No escape hatch in P1.
    Reject,
}

/// The single projection used by add, promote, and edit (plan §6).
///
/// Routing all three through one struct is what stops the warning threshold,
/// the rejection threshold, and the usage meter from drifting apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequiredBudgetProjection {
    /// Tokens the current Required set already spends.
    pub current_tokens: usize,
    /// Signed change this write would make; negative for demote or delete.
    pub delta_tokens: isize,
    /// `current_tokens + delta_tokens`, saturating at zero.
    pub projected_tokens: usize,
    /// The fixed reservation this is measured against.
    pub budget_tokens: usize,
}

impl RequiredBudgetProjection {
    /// Project from an already-rendered Required set, so the meter and the
    /// projection read the same tokens.
    pub fn from_rendered(current: &[RenderedRequiredMemory], delta_tokens: isize) -> Self {
        let counter = canonical_memory_counter();
        let current_tokens = current.iter().map(|m| counter.count(m.rendered())).sum();
        project_required_budget(current_tokens, delta_tokens)
    }

    /// Whether the projected state overflows the fixed budget.
    ///
    /// Strictly greater than, matching [`check_required_budget`]: a set that
    /// exactly fills the reservation still fits, so write-time and runtime
    /// never disagree at the boundary.
    pub fn is_over_budget(&self) -> bool {
        self.projected_tokens > self.budget_tokens
    }

    /// Whether the projection is inside the soft warning band.
    pub fn is_in_warning_band(&self) -> bool {
        self.projected_tokens as f64 >= self.budget_tokens as f64 * WARNING_RATIO
    }

    /// Apply plan §7's table for `op`.
    pub fn decide(&self, op: WriteOperation) -> WriteDecision {
        if self.is_over_budget() {
            return match op {
                // Edit is the one operation that may create overflow state:
                // locking it would trap a user whose only way out is to
                // rewrite or merge their instructions.
                WriteOperation::Edit => WriteDecision::AllowWithWarning,
                WriteOperation::Add | WriteOperation::Promote => WriteDecision::Reject,
            };
        }
        if self.is_in_warning_band() {
            WriteDecision::AllowWithWarning
        } else {
            WriteDecision::Allow
        }
    }
}

/// Build a projection from raw token counts (plan §6).
///
/// `delta_tokens` is signed: positive to add or grow a Required memory,
/// negative to demote or delete one. The projected total saturates at zero —
/// it is a count and can never be negative.
pub fn project_required_budget(
    current_tokens: usize,
    delta_tokens: isize,
) -> RequiredBudgetProjection {
    let projected_tokens = if delta_tokens >= 0 {
        current_tokens.saturating_add(delta_tokens as usize)
    } else {
        current_tokens.saturating_sub(delta_tokens.unsigned_abs())
    };

    RequiredBudgetProjection {
        current_tokens,
        delta_tokens,
        projected_tokens,
        budget_tokens: REQUIRED_BUDGET_TOKENS,
    }
}

/// A Required memory that has already been rendered for injection.
///
/// The rendered form — not bare `content` — is what gets counted, so wrapper,
/// prefix, and separator overhead is included (plan §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedRequiredMemory {
    memory_id: String,
    rendered: String,
}

impl RenderedRequiredMemory {
    pub fn new(memory_id: impl Into<String>, rendered: impl Into<String>) -> Self {
        Self {
            memory_id: memory_id.into(),
            rendered: rendered.into(),
        }
    }

    pub fn memory_id(&self) -> &str {
        &self.memory_id
    }

    pub fn rendered(&self) -> &str {
        &self.rendered
    }
}

/// Per-memory size, for the usage meter and the overflow recovery screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredMemoryUsage {
    pub memory_id: String,
    pub rendered_tokens: usize,
}

/// Outcome of checking the Required set against the fixed budget.
///
/// All-or-nothing by construction: there is no variant that can express a
/// partial Required set, so no caller can inject "what fits" (plan §1.3,
/// invariant 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequiredBudgetCheck {
    /// Every Required memory is injected, in input order.
    WithinBudget {
        usage: Vec<RequiredMemoryUsage>,
        total_tokens: usize,
        budget_tokens: usize,
    },
    /// Inference must be blocked. `required` lists *every* Required memory
    /// with its size, purely so the user can choose what to cut — it is a
    /// diagnostic report, never an injection set.
    Exceeded {
        required: Vec<RequiredMemoryUsage>,
        required_tokens: usize,
        required_budget_tokens: usize,
    },
}

/// The conservative max of tiktoken and the bytes/4 heuristic.
///
/// The heuristic undercounts CJK by roughly 2x (a 3-byte UTF-8 character is
/// often a whole token, but bytes/4 charges it 0.75), while tiktoken can
/// undercount text a different model tokenizes more finely. Taking the max
/// makes the budget safe for both.
struct CanonicalMemoryCounter {
    tiktoken: Option<TiktokenCounter>,
    heuristic: HeuristicCounter,
}

impl TokenCounter for CanonicalMemoryCounter {
    fn count(&self, text: &str) -> usize {
        let heuristic = self.heuristic.count(text);
        match &self.tiktoken {
            Some(t) => heuristic.max(t.count(text)),
            // tiktoken unavailable: the heuristic alone is the best we have.
            None => heuristic,
        }
    }
}

/// The single canonical counter for memory budgeting (plan invariant 5).
///
/// Used by write-time projection, the `/memories` usage display, and runtime
/// validation alike, so all three always agree. Never fails.
pub fn canonical_memory_counter() -> Box<dyn TokenCounter> {
    Box::new(CanonicalMemoryCounter {
        tiktoken: TiktokenCounter::new().ok(),
        heuristic: HeuristicCounter,
    })
}

/// Check a rendered Required set against the fixed budget.
///
/// Input order is preserved and never sorted: Required bypasses relevance,
/// top-K, recency, alphabetical order, and importance (plan invariant 1).
/// On overflow this returns `Exceeded` — the caller MUST block the turn and
/// MUST NOT call the model, inject a subset, drop the oldest or largest, or
/// auto-demote (plan §8).
pub fn check_required_budget(memories: &[RenderedRequiredMemory]) -> RequiredBudgetCheck {
    let counter = canonical_memory_counter();

    let usage: Vec<RequiredMemoryUsage> = memories
        .iter()
        .map(|m| RequiredMemoryUsage {
            memory_id: m.memory_id().to_string(),
            rendered_tokens: counter.count(m.rendered()),
        })
        .collect();

    let total_tokens: usize = usage.iter().map(|u| u.rendered_tokens).sum();

    if total_tokens <= REQUIRED_BUDGET_TOKENS {
        RequiredBudgetCheck::WithinBudget {
            usage,
            total_tokens,
            budget_tokens: REQUIRED_BUDGET_TOKENS,
        }
    } else {
        RequiredBudgetCheck::Exceeded {
            required: usage,
            required_tokens: total_tokens,
            required_budget_tokens: REQUIRED_BUDGET_TOKENS,
        }
    }
}
