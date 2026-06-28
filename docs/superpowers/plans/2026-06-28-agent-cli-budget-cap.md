# mur agent cli — Session Budget Cap — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `--budget-usd <X>` ceiling on a `mur agent cli` session: the footer shows spend against it, and once the session's estimated cost reaches the cap, a new turn is refused (with a clear message) instead of silently spending more.

**Architecture:** Reuse the footer's existing cost machinery. `App` already accumulates `session_in`/`session_out` (via `apply_usage`) and holds `pricing`; `footer::turn_cost(&Pricing, &UsageCounts)` already computes cost. Add `App.budget_usd: Option<f64>` (set in `run_tui` exactly like `auto_approve`), an `App::session_cost()` helper, a pre-turn gate in `submit()`, and a footer suffix showing `$spent / $budget`. No runtime change — this is a cli-side, attended safety net. Lazy v1: a pre-turn gate (no mid-turn abort), USD only (no `--budget-tokens`), restart to reset (no `/budget` command).

**Tech Stack:** Rust (edition 2024), the existing `footer::{Pricing, UsageCounts, turn_cost}`. No new dependency.

## Global Constraints

- **Independent cli feature** — branch from `main` (this plan was written on `feat/agent-cli-budget-cap`, cut off `main d99383a4` = the #527 tip with all prior cli work).
- **Rust edition 2024**; **no hardcoded values** (the budget is the user's flag value; no magic numbers); brand "MUR" uppercase in any user-facing copy.
- **Tests:** mur-core needs `ORT_STRATEGY=download`; toolchain cargo if rustup broken (`export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$HOME/.cargo/bin:$PATH"`, plain `cargo test`).
- **Lint gate:** `cargo clippy -p mur-core -- -D warnings` + `cargo fmt`.
- **Cost is an ESTIMATE** — `session_cost` is `None` when the agent's model has no pricing (inline models); the gate must FAIL OPEN (never block when cost is unknown) and the footer just omits the budget suffix.
- **Watch the test build** — adding a field to the `AgentAction::Cli` clap variant means every `Cli { … }` destructure (including `#[cfg(test)]` ones in `cli/agent.rs`) must add the field or `..`. A binary build won't catch a missed test destructure (E0027) — run `cargo test -p mur-core --no-run` (or a real `cargo test`) before claiming done.

---

### Task 1: `--budget-usd` flag + `App.budget_usd` + `session_cost()` helper

**Files:**
- Modify: `mur-core/src/cli/agent.rs` (the `Cli { … }` variant ~91-104; any `#[cfg(test)]` `Cli { … }` destructure)
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (`cmd_cli` ~93, `run_tui` ~170, dispatch call site)
- Modify: `mur-core/src/cmd/agent/cli/app.rs` (`App.budget_usd` field + ctor + `session_cost()`)
- Test: `app.rs` (inline — `session_cost`)

**Interfaces:**
- Produces: `App.budget_usd: Option<f64>`; `App::session_cost(&self) -> Option<f64>` (= `footer::turn_cost(&self.pricing, &UsageCounts{input: session_in, output: session_out})`). `cmd_cli(names, resume, auto, skin, plain, budget_usd)`.

- [ ] **Step 1: Write the failing test** (in `app.rs`'s test module)

```rust
    #[test]
    fn session_cost_uses_pricing_over_session_tokens_or_none() {
        let theme = crate::cmd::agent::cli::theme::resolve_skin("dark");
        let mut a = App::new(std::path::PathBuf::from("/tmp"), "x".into(), Session::ephemeral_for_test(), theme);
        a.pricing = super::footer::Pricing { in_per_1k: Some(3.0), out_per_1k: Some(15.0), window: None };
        a.session_in = 1000;
        a.session_out = 1000;
        // (1000/1000*3) + (1000/1000*15) = 18.0
        assert_eq!(a.session_cost(), Some(18.0));
        // unpriced model → None (gate must fail open)
        a.pricing = super::footer::Pricing::default();
        assert_eq!(a.session_cost(), None);
    }
```
> Build the `App` the way `app.rs`'s existing tests do (grep `App::new(` / a test-`Session` constructor in the test module — e.g. `apply_usage_accumulates_session_and_sets_turn` at ~1214 already builds one; mirror it exactly, including how it makes a `Session` and `Theme`). Adjust the expected `18.0` only if the real `turn_cost` formula differs.

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-core --lib session_cost 2>&1 | tail -15`
Expected: FAIL — `session_cost` / `budget_usd` not found.

- [ ] **Step 3: Add the field + helper** (`app.rs`)

Field (after `pricing` or near the session counters):
```rust
    /// Optional per-session USD ceiling (`--budget-usd`). When set, a new turn
    /// is refused once `session_cost()` reaches it. `None` = unlimited.
    pub budget_usd: Option<f64>,
```
Ctor init: `budget_usd: None,`.
Helper (near `apply_usage`):
```rust
    /// Estimated cumulative USD spent this session, or `None` when the model
    /// has no pricing (inline models) — callers must treat `None` as "unknown",
    /// never as zero.
    pub fn session_cost(&self) -> Option<f64> {
        super::footer::turn_cost(
            &self.pricing,
            &super::footer::UsageCounts { input: self.session_in, output: self.session_out },
        )
    }
```
> Confirm `UsageCounts`'s field names are `input`/`output` (they are, per `parse_usage`).

- [ ] **Step 4: Add the flag + thread it**

`cli/agent.rs` — in the `Cli { … }` variant, after `skin`/`plain`:
```rust
        /// Stop accepting new turns once this session's estimated cost (USD)
        /// reaches this ceiling. Omit for no limit.
        #[arg(long = "budget-usd")]
        budget_usd: Option<f64>,
```
Then update EVERY destructure of the variant: the dispatch call site (`grep -rn "cmd_cli(" mur-core/src` and the `Cli { … } =>` arm) and any `#[cfg(test)]` `let AgentAction::Cli { … }` in `cli/agent.rs` (add `budget_usd` or `budget_usd: _`).

`mod.rs cmd_cli` — add param `budget_usd: Option<f64>` and pass it to `run_tui`. Update `run_tui`'s signature with `budget_usd: Option<f64>`; after `build_app`, next to the `if auto { app.auto_approve = true; … }` block:
```rust
    app.budget_usd = budget_usd;
    if let Some(b) = budget_usd {
        app.push_system(format!(
            "session budget ${b:.2} — new turns stop once estimated spend reaches it"
        ));
    }
```
> The plain-mode path (`run_plain`) does NOT need the budget (it's a non-interactive/pipe path); just thread the param so `cmd_cli` compiles. Pass nothing extra to `run_plain`.

- [ ] **Step 5: Run test + full test BUILD (catch E0027)**

Run:
```
ORT_STRATEGY=download cargo test -p mur-core --lib session_cost 2>&1 | tail -6
ORT_STRATEGY=download cargo test -p mur-core --no-run 2>&1 | grep -E "error|Finished" | tail
```
Expected: test PASS; test build `Finished` with NO `error[E0027]` (proves every `Cli` destructure has the field). Then gate: `cargo clippy -p mur-core -- -D warnings && cargo fmt`. Verify the flag: `ORT_STRATEGY=download cargo run -p mur-core -- agent cli --help 2>&1 | grep budget`.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cli/agent.rs mur-core/src/cmd/agent/cli/mod.rs mur-core/src/cmd/agent/cli/app.rs
git commit -m "feat(cli): --budget-usd flag + App.budget_usd + session_cost() helper"
```

---

### Task 2: Pre-turn budget gate + footer display

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (`submit()` gate before `begin_user_turn`)
- Modify: `mur-core/src/cmd/agent/cli/ui.rs` (`render_status` / `fmt_footer` budget suffix)
- Test: `mur-core/src/cmd/agent/cli/mod.rs` or `app.rs` (a pure gate-decision helper)

**Interfaces:**
- Consumes: `App.budget_usd`, `App::session_cost()` (Task 1).
- Produces: `App::over_budget(&self) -> bool` (pure: `budget_usd` set AND `session_cost()` is `Some(spent)` AND `spent >= budget` — fails open when cost is `None`).

- [ ] **Step 1: Write the failing test** (the gate decision is the testable seam)

```rust
    #[test]
    fn over_budget_only_when_priced_and_at_or_past_cap() {
        let theme = crate::cmd::agent::cli::theme::resolve_skin("dark");
        let mut a = App::new(std::path::PathBuf::from("/tmp"), "x".into(), Session::ephemeral_for_test(), theme);
        a.pricing = super::footer::Pricing { in_per_1k: Some(3.0), out_per_1k: Some(15.0), window: None };
        a.session_in = 1000; a.session_out = 1000; // $18.00 spent
        a.budget_usd = None;            assert!(!a.over_budget());           // no cap
        a.budget_usd = Some(20.0);      assert!(!a.over_budget());           // under
        a.budget_usd = Some(18.0);      assert!(a.over_budget());            // at cap
        a.budget_usd = Some(5.0);       assert!(a.over_budget());            // over
        a.pricing = super::footer::Pricing::default(); // unpriced → fail OPEN
        a.budget_usd = Some(0.01);      assert!(!a.over_budget());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-core --lib over_budget 2>&1 | tail -10`
Expected: FAIL — `over_budget` not found.

- [ ] **Step 3: Implement `over_budget` + wire the gate**

In `app.rs`:
```rust
    /// True when a USD cap is set and the estimated session spend has reached
    /// it. Fails OPEN: an unpriced model (`session_cost() == None`) never blocks.
    pub fn over_budget(&self) -> bool {
        match (self.budget_usd, self.session_cost()) {
            (Some(cap), Some(spent)) => spent >= cap,
            _ => false,
        }
    }
```
In `mod.rs submit()`, immediately before `let task_id = app.begin_user_turn(&trimmed);` (and after the empty-input / slash / `!command` / streaming-steer handling, so those still work), add:
```rust
    // Session budget cap: refuse a NEW turn once estimated spend hits the cap.
    // (Does not interrupt an in-flight turn — that's handled above by the
    // streaming branch.) Fails open when the model has no pricing.
    if app.over_budget() {
        let cap = app.budget_usd.unwrap_or(0.0);
        let spent = app.session_cost().unwrap_or(0.0);
        app.push_system(format!(
            "↯ session budget reached — spent ~${spent:.2} of ${cap:.2}. Restart `mur agent cli` to reset."
        ));
        return;
    }
```
> Place it AFTER the `if app.streaming { … }` steer branch (so a mid-turn steer still works) and the input is NOT cleared on refusal (mirror how the streaming-reject path leaves the input intact — the user may want to copy it).

- [ ] **Step 4: Footer shows spend against the budget**

In `ui.rs render_status` (or the pure `fmt_footer` at ~376), when `app.budget_usd` is `Some(cap)`, append a budget suffix to the existing cost segment, e.g. `· $<spent> / $<cap>` (red/normal — match the footer's existing style; P1 footer is monochrome, so plain text is fine). Use `app.session_cost()` for spent (omit the `$<spent>` if `None`, just show `/ $<cap>`), formatted `{:.2}`. Keep `fmt_footer` a pure function — pass `budget_usd` + the session cost (or session tokens + pricing) in as params; do NOT call `App` methods from inside the pure formatter. Add/extend a `fmt_footer` unit test asserting the suffix appears when a budget is set and is absent otherwise.

- [ ] **Step 5: Run tests + gate**

Run:
```
ORT_STRATEGY=download cargo test -p mur-core --lib "over_budget" "fmt_footer" 2>&1 | grep -E "test result|FAILED"
ORT_STRATEGY=download cargo test -p mur-core --lib cmd::agent::cli 2>&1 | grep "test result"
ORT_STRATEGY=download cargo check -p mur-core && cargo clippy -p mur-core -- -D warnings && cargo fmt
```
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/agent/cli/mod.rs mur-core/src/cmd/agent/cli/ui.rs mur-core/src/cmd/agent/cli/app.rs
git commit -m "feat(cli): refuse new turns past --budget-usd + show spend/budget in the footer"
```

---

## Manual verification (after both tasks)

1. Build: `cargo build --release -p mur-core`.
2. `./target/release/mur agent cli <agent> --budget-usd 0.05` (a deliberately tiny cap on a priced agent).
3. Confirm the startup note + the footer showing `$<spent> / $0.05`.
4. Send turns until spend crosses $0.05; the next send is refused with `↯ session budget reached — spent ~$X of $0.05. Restart …`, the input is preserved, and no turn starts.
5. Sanity: without `--budget-usd`, no footer suffix, no gate. On an inline/unpriced model, `--budget-usd` never blocks (fail-open) — confirm a send still works.

## Out of scope (ponytail)

- `--budget-tokens` (USD is the headline; tokens later if wanted).
- A `/budget <X>` command to raise the cap live (restart resets it for v1).
- Mid-turn abort when a single turn would blow the cap (the pre-turn gate covers the common case; the in-flight turn already shows live cost in the footer).

## Self-Review (completed)

- **Spec coverage:** flag + field + `session_cost` (T1), gate + footer (T2). ✔
- **Placeholder scan:** none — code in every step; the fail-open behavior + lazy-v1 cuts are explicit decisions. ✔
- **Type consistency:** `budget_usd: Option<f64>` + `session_cost()`/`over_budget()` defined in T1/T2 and consumed in `submit()`/footer; `turn_cost`/`UsageCounts`/`Pricing` match the footer module. ✔
- **E0027 guard:** Task 1 Step 5 explicitly runs the test build (`--no-run`) to catch a missed `Cli` destructure — the exact class of bug that slipped through on the plain-mode branch. ✔
