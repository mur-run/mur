# Plan: bounded, not budgeted — `cost_usd` applies only to billable models

> Execute with **`mur-executing-plans`**. Spec:
> `docs/superpowers/specs/2026-09-12-execution-limits-design.md` §2 D4, §5, §9 step 2.
> Base: `main` after #1268 (step 1). No `limits:` schema yet — this step works
> on today's `loop.budget_usd` and `loop.deadline` fields.

**Goal.** A fleet whose models are local or subscription-billed is never asked
for a dollar figure: the loop ignores `budget_usd` for it, and unattended
auto-run accepts a deadline as the bound.

**Architecture.** `mur-common` learns to say what a `ModelEntry` costs
(`billing_or_inferred`). A new `mur-core::cmd::fleet::billing` module folds a
fleet's router and members into one verdict (`FleetBilling`) and defines
"bounded" (`is_bounded`). The loop and the daemon's `due_fleets` consult it
instead of `budget_usd > 0`.

**Tech stack.** Rust 2024, `cargo nextest`. `mur-core` env:
`ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432`;
from a worktree add `CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target`.

## Global Constraints (from the spec)

- D4: `cost_usd` (today `budget_usd`) exists **only when the model's BillingMode is UsageBilled**; absent or ignored for Subscription and Local.
- §5: unattended auto-run stays opt-in (`MUR_FLEET_AUTORUN=1`), kill-switch honoured, governance fail-closed, `yes:false` everywhere. Only the *bound* generalises: **`deadline` set, or `cost_usd` set on a billable model — at least one.**
- Unknown billing is treated as **billable** (conservative). The user is told, once, how to mark the model local.
- No new user-facing setting. `models.yaml`'s existing `billing:` field is the switch.
- Before every commit: `cargo fmt`, `cargo clippy -p <crate> --all-targets -- -D warnings` clean, the named tests green. `std::os::unix::` in a fixture sits under `#[cfg(unix)]`.

## File structure

| File | Responsibility | Task |
|---|---|---|
| `mur-common/src/model.rs` | `ModelEntry::billing_or_inferred()`; tests | 1 |
| `mur-core/src/cmd/fleet/billing.rs` (new) | `FleetBilling`, `fleet_billing_with()`, `fleet_billing()`, `is_bounded()`; tests | 2 |
| `mur-core/src/cmd/fleet/mod.rs` | `pub mod billing;` | 2 |
| `mur-core/src/cmd/fleet/loop_run.rs` | budget ignored for a non-billable fleet, with a one-line note; test | 3 |
| `mur-daemon/src/fleet_tick.rs` | `eligible()` replaces `has_budget`; tests | 4 |

---

## Task 1 — a `ModelEntry` can say how it is billed

**Interfaces.**
- Consumes: `pub enum BillingMode { Subscription, UsageBilled, Local }`, `ModelEntry { provider: String, billing: Option<BillingMode>, .. }` (exist).
- Produces: `impl ModelEntry { pub fn billing_or_inferred(&self) -> BillingMode }`.

### Steps

- [ ] **1.1 Write the failing test** — in `mur-common/src/model.rs`, in the test module that holds `entry_without_billing_metadata_stays_unknown`, add:

```rust
    /// Explicit `billing:` wins. Without it, the provider decides what can be
    /// decided — ollama runs here, codex/claude ride a subscription — and
    /// everything else is treated as metered, because guessing "free" is the
    /// one mistake a cost gate must not make.
    #[test]
    fn billing_is_inferred_from_the_provider_when_not_declared() {
        let mut e = ModelEntry {
            provider: "ollama".into(),
            model: "llama3.2:3b".into(),
            ..Default::default()
        };
        assert_eq!(e.billing_or_inferred(), BillingMode::Local);
        e.provider = "codex".into();
        assert_eq!(e.billing_or_inferred(), BillingMode::Subscription);
        e.provider = "claude".into();
        assert_eq!(e.billing_or_inferred(), BillingMode::Subscription);
        e.provider = "openai".into();
        assert_eq!(e.billing_or_inferred(), BillingMode::UsageBilled, "unknown is metered");
        e.provider = "anthropic".into();
        assert_eq!(e.billing_or_inferred(), BillingMode::UsageBilled);
        // A declaration overrides every inference — an LM Studio entry is
        // `provider: openai` and the user marks it local.
        e.billing = Some(BillingMode::Local);
        assert_eq!(e.billing_or_inferred(), BillingMode::Local);
    }
```

  If `ModelEntry` does not implement `Default`, build it the way `entry_without_billing_metadata_stays_unknown` does (deserialise a minimal YAML string) and set `provider`/`billing` on the result.

- [ ] **1.2 Watch it fail** — `cargo nextest run -p mur-common --lib -E 'test(billing_is_inferred)'`. Expected: `no method named billing_or_inferred`.

- [ ] **1.3 Add the method** — in `mur-common/src/model.rs`, in `impl ModelEntry` (create the block next to the struct if none exists):

```rust
    /// How this model is paid for, for the cost gates. An explicit `billing:`
    /// is the answer; without one the provider decides what it can:
    /// `ollama` runs on this machine, `codex` and `claude` ride a flat
    /// subscription. Everything else — including a loopback `base_url`, which
    /// is just as often the model gateway fronting a metered API — is treated
    /// as metered. Guessing "free" is the one mistake a cost gate must not
    /// make; a wrong "metered" costs the user one line in `models.yaml`
    /// (`billing: local`), and the gate says so when it applies.
    pub fn billing_or_inferred(&self) -> BillingMode {
        if let Some(b) = self.billing {
            return b;
        }
        match self.provider.as_str() {
            "ollama" => BillingMode::Local,
            "codex" | "claude" => BillingMode::Subscription,
            _ => BillingMode::UsageBilled,
        }
    }
```

  `BillingMode` must be `Copy` for `if let Some(b) = self.billing`; if it is not, add `Copy` to its derive list (it is a fieldless enum).

- [ ] **1.4 Watch it pass** — `cargo nextest run -p mur-common --lib -E 'test(/billing/)'`. Expected: all pass. Then `cargo clippy -p mur-common --all-targets -- -D warnings`, `cargo fmt -p mur-common`.

  **Hub check (mandatory — the Hub is workspace-excluded):** this task adds a method and possibly a derive; it changes no field, so no Hub struct literal breaks. Confirm with `command grep -rn "ModelEntry {" mur-hub-gui/src-tauri/src | wc -l` (informational) and `cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml` if `ui/dist` exists; otherwise note it in the commit body.

- [ ] **1.5 Commit**:

```
feat(model): ModelEntry::billing_or_inferred — provider decides what it can

Explicit `billing:` wins; ollama is Local, codex/claude are Subscription,
everything else is UsageBilled. Unknown is metered on purpose: a loopback
base_url is as often the gateway fronting a metered API as it is a local
runtime, and guessing "free" is the one mistake a cost gate must not make.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 2 — one verdict per fleet, and the definition of "bounded"

**Interfaces.**
- Consumes: `ModelEntry::billing_or_inferred()` (Task 1), `Fleet { router: Option<String>, members: Vec<String>, loop_cfg: Option<FleetLoop>, .. }`, `Fleet::router_or_concierge()`, `FleetLoop { budget_usd: f64, deadline: String, .. }`, `mur_common::agent::AgentProfile::load(mur_home, name) -> anyhow::Result<AgentProfile>` with `.model_ref: Option<String>`, `mur_common::model::ModelRegistry::load_from(&Path)` with `.models: BTreeMap<String, ModelEntry>`.
- Produces (all in `mur-core/src/cmd/fleet/billing.rs`):

```rust
pub struct FleetBilling { pub billable: bool, pub unknown: Vec<(String, String)> }
pub fn fleet_billing_with(fleet: &Fleet, lookup: impl Fn(&str) -> Option<BillingMode>) -> FleetBilling
pub fn fleet_billing(mur_home: &Path, fleet: &Fleet) -> FleetBilling
pub fn is_bounded(lc: Option<&FleetLoop>, billing: &FleetBilling) -> bool
pub fn unbounded_reason(lc: Option<&FleetLoop>, billing: &FleetBilling, fleet: &str) -> String
```

### Steps

- [ ] **2.1 Create the module with its tests** — new file `mur-core/src/cmd/fleet/billing.rs`:

```rust
//! Whether a fleet costs money, and what "bounded" means for it (spec
//! 2026-09-12 execution-limits, §2 D4 and §5).
//!
//! A fleet is billable when ANY agent it runs — router or member — resolves to
//! a metered model, or to one whose billing cannot be resolved at all. That is
//! the conservative reading: the cost gate exists to stop unbounded spend, so
//! it errs toward "this might cost something". A local-only fleet is bounded by
//! a deadline; a billable one by a deadline or a `budget_usd`.

use std::path::Path;

use mur_common::fleet::{Fleet, FleetLoop};
use mur_common::model::BillingMode;

/// The fold of every agent's billing into one answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetBilling {
    /// True when at least one agent is `UsageBilled`, or unresolvable.
    pub billable: bool,
    /// `(agent, model_ref)` pairs whose billing could not be resolved and were
    /// therefore counted as metered — so the user can be told which line in
    /// `models.yaml` to fix.
    pub unknown: Vec<(String, String)>,
}

/// The pure core: `lookup(agent)` answers `Some(mode)` for a resolvable agent
/// and `None` for one whose profile or model cannot be read.
pub fn fleet_billing_with(fleet: &Fleet, lookup: impl Fn(&str) -> Option<BillingMode>) -> FleetBilling {
    let mut billable = false;
    let mut unknown = Vec::new();
    let router = fleet.router_or_concierge().to_string();
    for agent in std::iter::once(router.as_str()).chain(fleet.members.iter().map(String::as_str)) {
        match lookup(agent) {
            Some(BillingMode::UsageBilled) => billable = true,
            Some(BillingMode::Local) | Some(BillingMode::Subscription) => {}
            None => {
                billable = true;
                unknown.push((agent.to_string(), String::from("?")));
            }
        }
    }
    FleetBilling { billable, unknown }
}

/// `fleet_billing_with` against the real `~/.mur`: each agent's
/// `profile.yaml` `model_ref` → `models.yaml` entry → `billing_or_inferred`.
pub fn fleet_billing(mur_home: &Path, fleet: &Fleet) -> FleetBilling {
    let registry = mur_common::model::ModelRegistry::load_from(&mur_home.join("models.yaml")).ok();
    let mut out = fleet_billing_with(fleet, |agent| {
        let profile = mur_common::agent::AgentProfile::load(mur_home, agent).ok()?;
        let model_ref = profile.model_ref?;
        let entry = registry.as_ref()?.models.get(&model_ref)?;
        Some(entry.billing_or_inferred())
    });
    // Fill in the model_ref for the unknowns so the note can name it.
    for (agent, model_ref) in out.unknown.iter_mut() {
        if let Ok(p) = mur_common::agent::AgentProfile::load(mur_home, agent)
            && let Some(m) = p.model_ref
        {
            *model_ref = m;
        }
    }
    out
}

/// §5: unattended work must be bounded. A deadline bounds any fleet; a
/// positive `budget_usd` bounds a billable one and means nothing for a fleet
/// that cannot spend.
pub fn is_bounded(lc: Option<&FleetLoop>, billing: &FleetBilling) -> bool {
    let Some(l) = lc else { return false };
    let has_deadline = !l.deadline.trim().is_empty();
    let has_budget = billing.billable && l.budget_usd > 0.0;
    has_deadline || has_budget
}

/// Why `is_bounded` said no, in the words of the fix. Only meaningful when it
/// did say no.
pub fn unbounded_reason(lc: Option<&FleetLoop>, billing: &FleetBilling, fleet: &str) -> String {
    let _ = lc;
    if billing.billable {
        format!(
            "fleet '{fleet}' has no bound — set a deadline (mur fleet settings {fleet} --deadline 2h) \
             or a budget (--budget-usd <USD>) before it may run unattended"
        )
    } else {
        format!(
            "fleet '{fleet}' has no bound — it runs on local/subscription models, so a budget does not \
             apply; set a deadline: mur fleet settings {fleet} --deadline 2h"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fleet(router: Option<&str>, members: &[&str], lc: Option<FleetLoop>) -> Fleet {
        Fleet {
            name: "dev".into(),
            display_name: String::new(),
            goal: "g".into(),
            router: router.map(str::to_string),
            members: members.iter().map(|m| m.to_string()).collect(),
            team_id: None,
            channel_id: "fleet-dev".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg: lc,
            parallel: None,
            hitl: None,
            requires_programs: vec![],
        }
    }

    fn lc(budget_usd: f64, deadline: &str) -> FleetLoop {
        FleetLoop {
            trigger: "interval:1h".into(),
            max_iterations: 0,
            budget_usd,
            deadline: deadline.into(),
            done_when: String::new(),
        }
    }

    #[test]
    fn a_fleet_is_billable_when_any_agent_is_metered_or_unknown() {
        let f = fleet(Some("mur"), &["a", "b"], None);
        let all_local = fleet_billing_with(&f, |_| Some(BillingMode::Local));
        assert!(!all_local.billable);
        assert!(all_local.unknown.is_empty());

        let one_metered = fleet_billing_with(&f, |a| {
            Some(if a == "b" { BillingMode::UsageBilled } else { BillingMode::Local })
        });
        assert!(one_metered.billable);

        let subscription = fleet_billing_with(&f, |_| Some(BillingMode::Subscription));
        assert!(!subscription.billable, "a subscription fleet cannot spend");

        // The router counts: a local member fleet with a metered router bills.
        let metered_router = fleet_billing_with(&f, |a| {
            Some(if a == "mur" { BillingMode::UsageBilled } else { BillingMode::Local })
        });
        assert!(metered_router.billable);

        // Unresolvable → metered, and named.
        let unknown = fleet_billing_with(&f, |a| (a != "a").then_some(BillingMode::Local));
        assert!(unknown.billable);
        assert_eq!(unknown.unknown, vec![("a".to_string(), "?".to_string())]);
    }

    #[test]
    fn bounded_means_a_deadline_or_a_budget_that_can_apply() {
        let local = FleetBilling { billable: false, unknown: vec![] };
        let billed = FleetBilling { billable: true, unknown: vec![] };

        // No loop config at all: never bounded.
        assert!(!is_bounded(None, &local));
        assert!(!is_bounded(None, &billed));

        // A deadline bounds anyone.
        assert!(is_bounded(Some(&lc(0.0, "2h")), &local));
        assert!(is_bounded(Some(&lc(0.0, "2h")), &billed));

        // A budget bounds only a fleet that can spend.
        assert!(is_bounded(Some(&lc(5.0, "")), &billed));
        assert!(!is_bounded(Some(&lc(5.0, "")), &local), "a dollar figure means nothing here");

        // Neither → unbounded, whatever the billing.
        assert!(!is_bounded(Some(&lc(0.0, "")), &billed));
        assert!(!is_bounded(Some(&lc(0.0, "   ")), &local), "whitespace is not a deadline");
    }

    #[test]
    fn the_unbounded_reason_names_the_knob_that_applies() {
        let local = FleetBilling { billable: false, unknown: vec![] };
        let billed = FleetBilling { billable: true, unknown: vec![] };
        let r = unbounded_reason(Some(&lc(0.0, "")), &local, "dev");
        assert!(r.contains("--deadline"), "{r}");
        assert!(!r.contains("--budget-usd"), "a local fleet is not offered a budget: {r}");
        let r = unbounded_reason(Some(&lc(0.0, "")), &billed, "dev");
        assert!(r.contains("--deadline") && r.contains("--budget-usd"), "{r}");
    }
}
```

- [ ] **2.2 Register the module** — in `mur-core/src/cmd/fleet/mod.rs`, next to `pub mod loop_run;` add `pub mod billing;`.

- [ ] **2.3 Watch it pass** — `cargo nextest run -p mur-core --lib -E 'test(/fleet::billing::/)'`. Expected: 3 pass. (The red step for this task is the compile error before 2.2 registers the module; run 2.3 before 2.2 if you want to see it.) If `let ... && let` in `fleet_billing` fails to compile, the crate is on an older edition than 2024 — it is not; if `AgentProfile::load` needs a different path type, follow its signature (`&Path`, `&str`).

- [ ] **2.4 fmt + clippy** (`cargo clippy -p mur-core --all-targets -- -D warnings`), then **commit**:

```
feat(fleet): one billing verdict per fleet, and what "bounded" means

`fleet_billing` folds router + members through `billing_or_inferred`: a
fleet is billable when any agent is metered or unresolvable (conservative).
`is_bounded` is the spec's §5 rule — a deadline bounds any fleet, a budget
bounds only one that can spend — and `unbounded_reason` says which knob
applies, never offering a dollar figure to a fleet that cannot spend.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 3 — the loop ignores a budget the fleet cannot spend

**Interfaces.**
- Consumes: `fleet_billing(mur_home, &fleet) -> FleetBilling` (Task 2), `effective_budget(flag, &fleet) -> Option<f64>` (exists, `loop_run.rs`), `RunProgress.budget_usd: Option<f64>` (exists).
- Produces: in `run_guarded`, `budget` is `None` for a non-billable fleet, `progress.budget_usd` is `None`, and one note is printed. A new pure helper `fn budget_for(fleet_budget: Option<f64>, billing: &FleetBilling) -> Option<f64>`.

### Steps

- [ ] **3.1 Write the failing test** — in `loop_run.rs`'s `mod tests`:

```rust
    /// A local or subscription fleet has no spend, so a `budget_usd` on it is
    /// noise: the guard would stop a run on a projection of dollars nobody is
    /// paying. Unknown billing keeps the budget — conservative, like the fold.
    #[test]
    fn a_budget_applies_only_to_a_fleet_that_can_spend() {
        use super::super::billing::FleetBilling;
        let local = FleetBilling { billable: false, unknown: vec![] };
        let billed = FleetBilling { billable: true, unknown: vec![] };
        assert_eq!(budget_for(Some(5.0), &billed), Some(5.0));
        assert_eq!(budget_for(Some(5.0), &local), None);
        assert_eq!(budget_for(None, &billed), None);
        assert_eq!(budget_for(None, &local), None);
    }
```

- [ ] **3.2 Watch it fail** — `cargo nextest run -p mur-core --lib -E 'test(a_budget_applies_only)'`. Expected: `cannot find function budget_for`.

- [ ] **3.3 Add the helper and use it** — below `fn effective_budget` in `loop_run.rs`:

```rust
/// The budget the guard enforces: the configured one for a fleet that can
/// spend, none for a fleet that cannot (spec D4). Enforcing a dollar ceiling
/// on local models stopped real runs on a projection of money nobody was
/// paying — the failure mode this exists to remove.
fn budget_for(fleet_budget: Option<f64>, billing: &super::billing::FleetBilling) -> Option<f64> {
    if billing.billable { fleet_budget } else { None }
}
```

  In `run_guarded`, replace `let budget = effective_budget(budget_usd, &fleet);` with:

```rust
    let billing = super::billing::fleet_billing(mur_home, &fleet);
    let configured_budget = effective_budget(budget_usd, &fleet);
    let budget = budget_for(configured_budget, &billing);
    if configured_budget.is_some_and(|b| b > 0.0) && budget.is_none() {
        println!(
            "  ℹ budget_usd ignored — fleet '{name}' runs on local/subscription models and cannot spend; \
             its bound is the deadline"
        );
    }
    if !billing.unknown.is_empty() {
        let who: Vec<String> = billing
            .unknown
            .iter()
            .map(|(a, m)| format!("{a} ({m})"))
            .collect();
        println!(
            "  ⚠ billing unknown for {} — treated as metered. Mark a local model with `billing: local` in models.yaml",
            who.join(", ")
        );
    }
```

  `progress.budget_usd` is already set from `budget` a few lines down (`budget_usd: budget,`); leave it — a non-billable fleet now records `None`.

- [ ] **3.4 Watch it pass** — `cargo nextest run -p mur-core --lib -E 'test(/loop_run::/)'`. Expected: all pass, including the step-1 tests (their fleets have no `models.yaml`, so billing is unknown → billable → behaviour unchanged).

- [ ] **3.5 fmt + clippy**, then **commit**:

```
fix(fleet): the loop does not enforce a budget a fleet cannot spend

A local or subscription fleet has no spend; a `budget_usd` on it stopped
runs on a projection of dollars nobody was paying. The guard now takes the
budget only when `fleet_billing` says the fleet is billable, says so once
when it drops one, and names any agent whose billing it could not resolve.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 4 — unattended auto-run requires a bound, not a dollar

**Interfaces.**
- Consumes: `is_bounded`, `fleet_billing`, `unbounded_reason` (Task 2).
- Produces: in `mur-daemon/src/fleet_tick.rs`, `fn eligible(lc: Option<&FleetLoop>, billing: &FleetBilling) -> bool` (a thin, testable name for `is_bounded` at the daemon's seam); `due_fleets` uses it in place of `has_budget`, and logs `unbounded_reason` at `warn` once per tick for a due-by-trigger fleet it skips.

### Steps

- [ ] **4.1 Write the failing tests** — in `fleet_tick.rs`'s test module, after `due_fleets_filters_by_trigger_and_last_run`:

```rust
    /// §5 as tested at the daemon's seam: a deadline bounds any fleet; a
    /// budget bounds only a billable one; neither means no unattended run.
    /// These three replace "positive budget required".
    #[test]
    fn eligibility_is_bounded_not_budgeted() {
        use mur_core::cmd::fleet::billing::FleetBilling;
        let local = FleetBilling { billable: false, unknown: vec![] };
        let billed = FleetBilling { billable: true, unknown: vec![] };
        let lc = |budget: f64, deadline: &str| FleetLoop {
            trigger: "interval:1m".into(),
            max_iterations: 0,
            budget_usd: budget,
            deadline: deadline.into(),
            done_when: String::new(),
        };
        // local fleet + deadline → runs; local fleet + budget only → does not
        assert!(eligible(Some(&lc(0.0, "2h")), &local));
        assert!(!eligible(Some(&lc(9.0, "")), &local));
        // billable fleet: either knob
        assert!(eligible(Some(&lc(9.0, "")), &billed));
        assert!(eligible(Some(&lc(0.0, "2h")), &billed));
        // neither knob, or no loop block: never
        assert!(!eligible(Some(&lc(0.0, "")), &billed));
        assert!(!eligible(None, &local));
    }

    /// The kill-switch still wins over a bounded, due fleet.
    #[test]
    fn kill_switch_beats_a_bounded_due_fleet() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let mut f = loop_fleet("auto", "interval:1m");
        f.loop_cfg.as_mut().unwrap().deadline = "2h".into();
        f.loop_cfg.as_mut().unwrap().budget_usd = 0.0;
        store::save_fleet(home, &f).unwrap();
        // No models.yaml in this home → billing unknown → billable; the
        // deadline is what makes it bounded here.
        assert_eq!(due_fleets(home, 5000).unwrap(), vec!["auto".to_string()]);
        mur_core::cmd::fleet::control::cmd_fleet_stop(home, "auto").unwrap();
        assert!(due_fleets(home, 5000).unwrap().is_empty(), "kill-switch must win");
    }
```

  `loop_fleet` and `store` already exist in this test module. Note the fixture comment `// positive so auto-run eligibility holds in tests` on `budget_usd: 1.0` stays true (unknown billing → billable → budget bounds it).

- [ ] **4.2 Watch it fail** — `cargo nextest run -p mur-daemon --lib -E 'test(/eligibility_is_bounded|kill_switch_beats/)'`. Expected: `cannot find function eligible`.

- [ ] **4.3 Replace the gate** — in `due_fleets`, replace from `// Auto-run requires a positive budget` through `let has_budget = …;` with:

```rust
        // Auto-run requires a BOUND (spec §5): a deadline for any fleet, or a
        // budget for one that can spend. "Positive budget" was the old rule;
        // it demanded a dollar figure from local-model fleets, which protected
        // nothing and blocked real users.
        let billing = mur_core::cmd::fleet::billing::fleet_billing(mur_home, &fleet);
        let bounded = eligible(lc, &billing);
```

  and replace `if has_budget` with `if bounded`. Then, so a fleet that is due but unbounded is not silently skipped forever, add inside the loop after the `commander_halted` computation:

```rust
        if !bounded && is_due(trigger, read_last_run(mur_home, &name), now_unix) {
            tracing::warn!(
                fleet = %name,
                "{}",
                mur_core::cmd::fleet::billing::unbounded_reason(lc, &billing, &name)
            );
        }
```

  Add the seam function next to `autorun_flag`:

```rust
/// The daemon's eligibility rule, by its spec name. Thin on purpose: the
/// definition lives in `mur_core::cmd::fleet::billing::is_bounded` so the loop
/// and the daemon cannot drift.
fn eligible(
    lc: Option<&FleetLoop>,
    billing: &mur_core::cmd::fleet::billing::FleetBilling,
) -> bool {
    mur_core::cmd::fleet::billing::is_bounded(lc, billing)
}
```

  If `FleetLoop` is not imported at the top of `fleet_tick.rs`, add `use mur_common::fleet::FleetLoop;`.

- [ ] **4.4 Watch it pass** — `cargo nextest run -p mur-daemon --lib`. Expected: all pass. `due_fleets_filters_by_trigger_and_last_run` and `cron_fleet_not_due_immediately_after_baseline` still pass because their fixture's `budget_usd: 1.0` bounds an unknown-billing (→ billable) fleet.

- [ ] **4.5 fmt + clippy** (`cargo clippy -p mur-daemon --all-targets -- -D warnings`), then **commit**:

```
fix(daemon): unattended auto-run requires a bound, not a positive budget

The safety triad's second leg was `budget_usd > 0`. Its intent is "no
unbounded unattended run"; a local-model fleet's spend is zero by
construction, so the dollar figure protected nothing and blocked real
users. The gate now asks `is_bounded`: a deadline for any fleet, or a
budget for one that can spend. Opt-in, kill-switch, governance fail-closed
and `yes:false` are unchanged. A due-but-unbounded fleet is logged with
the knob that would fix it instead of being skipped in silence.

Approved by the user 2026-09-12 (spec §5).

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## After the last task

- One PR: `feat: bounded, not budgeted — cost gates apply only to billable fleets (spec step 2)`. Body: the four commits, the §5 approval line, and the manual check.
- Manual check after `build.sh --install`: on this machine `dr_worker_*` run `claude_haiku` (Subscription) and the concierge runs `codex` — `mur fleet run deep-research --loop --max-iterations 1` must print `ℹ budget_usd ignored — … cannot spend` and never stop on `Budget`; with `MUR_FLEET_AUTORUN=1`, a fleet with `deadline: 2h` and `budget_usd: 0` on those agents appears in `due_fleets` (daemon log), one with neither is logged with the `--deadline` remedy.
- Memory: `feedback_autonomous_loop_safety_audit` already carries the 2026-09-12 amendment; add the PR number to it.
- Docs via `update-docs`: the fleet-loop page's guard paragraph — "a budget applies only to fleets on metered models; a local fleet is bounded by its deadline".
