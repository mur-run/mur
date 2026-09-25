# murmur /memories P1 — Required vs BestEffort injection

Source: user-authored P1 spec (this file is the canonical plan of record).

## 0. Problem

Permanent user instructions ("永遠用中文回覆我") compete in the same ranked,
truncated retrieval pool as incidental memories, so they can be silently
omitted from model context. `mur-agent-runtime/src/skills/injector.rs` sorts
notes by name and truncates on `max_in_prompt` + a character cap — a permanent
instruction survives or dies by alphabetical luck.

## 1. Core principles

1. **Required ≠ compliance.** `InjectionPolicy::Required` guarantees presence in
   the prompt, not that the model obeys. Keep this wording in code comments,
   architecture docs, and UI copy.
2. **Required never competes.** No relevance ranking, no top-K, no recency, no
   alphabetical order, no importance competition. Only BestEffort competes.
3. **No partial injection.** If the Required set does not fit the reserved
   budget, block the turn — never inject a subset.

## 2. P1 scope

In: Required/BestEffort split · Global scope only · Required-first injection ·
fixed Required budget · blocking overflow · two explicit creation entries ·
migration · budget UX.

Out: project scope, TurnContext, evidence/provenance, semantic applicability,
Guarded memory, embedding redesign, policy-slot parsing, conflict detection,
supersede chains, effective windows, `review_at`, volatility, consolidation,
turn-local skip, partial injection, automatic retry.

## 3. Data model

```rust
struct Memory {
    id: MemoryId,
    /// Opaque natural-language instruction in P1.
    content: String,
    /// Controls the injection guarantee, not model compliance.
    injection_policy: InjectionPolicy,
    /// P1 intentionally supports only Global scope; the enum is the
    /// extension point for Project scope in P2.
    scope: MemoryScope,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

enum InjectionPolicy { Required, BestEffort }

enum MemoryScope { Global /* P2: Project(ProjectId) */ }
```

## 4. Terminology (fixed)

| Internal | UI | Meaning |
|---|---|---|
| `InjectionPolicy::Required` | Permanent instructions | Injected every turn |
| `InjectionPolicy::BestEffort` | Remembered information | Injected when relevant |

Banned parallel vocabulary: hard / pinned / guaranteed / always memory. "Pinned"
may only ever be a presentation feature, never an injection policy.

## 5. Guardrails

```rust
const MAX_REQUIRED_MEMORY_CHARS: usize = 2000;   // per-memory validation error
const REQUIRED_MEMORY_BUDGET_TOKENS: usize = /* P1 value */;
const REQUIRED_MEMORY_COUNT_WARNING: usize = 50; // soft, never blocks
```

Budget is a **fixed reservation**, not "whatever context is left" — a permanent
instruction must not work on turn 1 and silently fail on turn 50. BestEffort
yields to Required, never the reverse. Document the derivation of the budget
value (10–20 typical instructions × 100–300 tokens + headroom) in the source
comment so it does not become an unsourced magic number. Per-model budget
profiles are P2; P1 picks the most conservative tokenizer as canonical.

## 6. Token accounting

```rust
trait MemoryTokenCounter { fn count_tokens(&self, rendered: &str) -> usize; }
```

One implementation shared by: add, promote, edit, 80% warning, `/memories`
usage display, runtime validation, overflow error. Count
`render_for_injection(memory)` — including wrapper/prefix/separator overhead —
never bare `content`.

```rust
struct RequiredBudgetProjection {
    current_tokens: usize,
    delta_tokens: isize,
    projected_tokens: usize,
    budget_tokens: usize,
}
```

Add / promote / edit all go through this one projection.

## 7. Write-time behavior

| Operation | < 80% | 80–100% | > 100% |
|---|---|---|---|
| Add instruction | create | create + soft warning | **reject** (no "Add anyway") |
| Make permanent | promote | promote + warning | **reject**, stays BestEffort |
| Edit Required | save | save + warning | warn + **Save anyway** allowed |

Edit is the only operation that may deliberately create overflow state — users
merging or rewriting instructions must not be locked out of editing. Saving into
overflow immediately enters the blocking state.

Demotion (`Remember only when relevant`) and delete of a Required memory both
require explicit confirmation. Nothing — overflow, inactivity, recency,
importance, classifier — may auto-demote.

## 8. Injector pipeline

```
all active memories
  ├─ Required ──► render_for_injection ──► token count
  │                 ├─ fits fixed budget ──► inject ALL
  │                 └─ else ──► RequiredBudgetExceeded ──► BLOCK INFERENCE
  └─ BestEffort ──► existing relevance/ranking ──► top-K ──► remaining budget
```

Minimum change to existing code: stop running one `sort().take(n)` over all
notes; partition first, and let only BestEffort reach the ranking path.

```rust
enum InjectionResult {
    Success {
        injected_required: Vec<MemoryId>,
        injected_best_effort: Vec<MemoryId>,
        omitted_best_effort: Vec<MemoryId>,
    },
    RequiredBudgetExceeded {
        required: Vec<RequiredMemoryUsage>,
        required_tokens: usize,
        required_budget_tokens: usize,
    },
}

struct RequiredMemoryUsage { memory_id: MemoryId, rendered_tokens: usize }
```

On overflow the system MUST NOT: call the model, partially inject, take first N,
sort by importance/recency, drop oldest/largest, auto-demote, or continue
without Required. It MUST return `RequiredBudgetExceeded`, block, and surface
recovery UI.

## 9. Blocking UX

- Overlay in the originating conversation (not a redirect), showing usage
  (`4,320 / 3,500 tokens`), a **Manage permanent instructions** action, and an
  explicit "Your message has not been sent."
- **Composer preservation:** the blocked message stays in the composer; the
  model was never called. No automatic retry — the user presses Send again.
- **Return path:** the manage screen carries `return_to` and offers
  **Back to conversation**.
- Manage screen in overflow mode shows "reduce by at least ~N tokens", per-item
  estimated size, and Edit / Remember only when relevant / Delete. It may sort
  or highlight by size — it must never judge importance.
- **No escape hatch in P1:** no skip-for-this-reply, continue-anyway,
  inject-what-fits, or temporary override. Those bring turn-local state,
  override lifecycle, and audit semantics — P2 territory.

## 10. Migration

All existing memories → `injection_policy = BestEffort`, `scope = Global`, even
when the content reads like a permanent instruction. Required may only arise
from explicit user action. One-time, non-blocking, generic notice announcing
permanent instructions; it recommends no specific memory and modifies nothing.

## 11. `/memories` layout

Two sections with two distinct creation entries — the system never guesses the
contract level:

- **Permanent instructions** — "always added to the AI's context", usage meter,
  `[+ Add instruction]`, per item Edit / Remember only when relevant / Delete.
- **Remembered information** — "may be used when relevant",
  `[+ Remember this]`, per item Make permanent / Edit / Delete.

## 12. Invariants

1. Required bypasses relevance, top-K, recency, alphabetical sort, importance.
2. No silent omission: either every Required memory is injected, or inference is
   blocked with `RequiredBudgetExceeded`.
3. Required is created only by explicit user action (Add instruction / Make
   permanent) — never by migration, classifier, score, or the LLM.
4. Required means injection, not compliance.
5. One rendered representation, one tokenizer, one budget profile across
   write-time projection, UI display, and runtime validation.

## 13. Implementation order

1. Schema: `InjectionPolicy`, `MemoryScope`, `updated_at`, migration.
2. Injector split — Required bypasses top-K. **Original bug is fixed here.**
3. Token accounting: `render_for_injection()` + `MemoryTokenCounter` + budget.
4. Runtime blocking: `RequiredBudgetExceeded`, model call provably skipped.
5. `/memories` two-section UI with two creation entries.
6. Write-time projection: add warn/reject, promote reject, edit warn.
7. Blocking UX: overlay, composer preservation, return path.
8. One-time migration notice.
9. Confirmation UX for delete and demotion.
10. Acceptance + regression tests.

## 14. Acceptance tests

1. **Original bug** — 50 BestEffort + 1 Required, top-K 10 → Required injected
   regardless of DB ordering.
2. **Required exceeds top-K** — 20 Required, top-K 10 → all 20 considered.
3. **Fits budget** — 2,800 / 3,500 → all injected, model called.
4. **Runtime overflow** — 4,320 / 3,500 → `RequiredBudgetExceeded`, model not called.
5. **No partial fallback** — 19 of 20 fit → inference blocked, zero injected subset.
6. **Add warning** — projected 2,900 / 3,500 (82.9%) → warn, creation allowed.
7. **Add overflow** — 3,200 + 500 / 3,500 → rejected, memory not created.
8. **Promotion overflow** — rejected, memory stays BestEffort.
9. **Edit overflow** — warning, Save anyway allowed, state becomes overflow,
   next Send blocks.
10. **Demotion recovery** — demote a 1,000-token Required → 3,320, blocking clears.
11. **Composer preservation** — blocked send keeps the typed message; after
    cleanup, return to conversation with the message intact.
12. **Migration** — "永遠使用中文" migrates to BestEffort.
13. **Separate creation paths** — same content is Required via Add instruction,
    BestEffort via Remember this; no reclassification.
14. **Token consistency** — write projection, UI usage, runtime validation agree.
15. **Delete confirmation** — Cancel no-ops, Confirm deletes.
16. **Count warning** — at threshold, warn only; creation still governed by tokens.

Regression test to keep forever: `"永遠用中文回答我"` + many BestEffort +
top-K = 10, so no future retrieval refactor can reintroduce the bug.

## 15. Definition of done

Required/BestEffort exists · Required bypasses top-K · no silent truncation ·
fixed budget · single tokenizer everywhere · add >100% rejected · promote >100%
rejected · edit >100% warns · runtime overflow blocks with no model call · input
never lost · two-section `/memories` with two entries · migration creates no
Required · delete and demotion confirmed · no project scope / TurnContext /
classifier / Guarded / temporary override · original bug has a regression test.

## 16. Later

- **P2:** project scope, TurnContext resolution, multi-source evidence, sticky
  visible scope, write-time admission control, append-only supersede, conflict
  management, effective windows, auditability.
- **P3:** Guarded preferences, semantic applicability, embedding relevance,
  policy-slot parsing, automatic conflict detection, consolidation, review
  scheduling, agent-managed memory.

If an implementation PR starts needing any of these, it has left P1 scope.
