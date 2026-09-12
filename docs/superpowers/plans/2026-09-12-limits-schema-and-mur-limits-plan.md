# Plan: the `limits:` schema, three-scope resolution, and `mur limits`

> Execute with **`mur-executing-plans`**. Spec:
> `docs/superpowers/specs/2026-09-12-execution-limits-design.md` §3.1, §3.3, §3.9, §6; §9 step 3.
> Base: `main` after #1271 (step 2 — `fleet::billing` exists). This step
> **adds** the schema and the command; it does **not** change what governs a
> run. The old caps (`hitl.max_iterations`, `hitl.max_tokens`,
> `loop.max_iterations`) keep applying until step 4 flips the runtime, and
> `mur limits` says so.

**Goal.** One `limits:` block with the same shape at three scopes, one
resolver that says what is in force and where it came from, and one command
that prints it — so a user can see every knob before step 4 makes them govern.

**Architecture.** `mur_common::limits` owns the schema (`Limits`), the
duration grammar, the built-in defaults and the pure resolver
(`resolve(global, fleet, agent, flags)` → per-knob value + source). The three
scope structs gain a `limits` field. `mur-core` composes the resolver with
`fleet::billing` for `cost_usd` applicability and prints it (`mur limits`);
writers (`mur fleet limits`, `mur agent limits`, `mur limits --global`) edit
exactly one scope, and the global one edits `config.yaml` textually.

**Tech stack.** Rust 2024, `cargo nextest`. `mur-core` env:
`ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432`;
from a worktree add `CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target`.

## Global Constraints (from the spec)

- §3.1: one schema — `deadline`, `stuck`, `cost_usd` — at `config.yaml limits:` → `fleet.yaml limits:` → `profile.yaml limits:` → per-call flag. Inner scopes **replace** a key; they never combine. A key absent at every scope takes the built-in default.
- §3.3: built-in defaults are constants in `mur_common::limits`, printed as `(built-in default)`.
- §3.9: `mur limits <agent|fleet>` prints every knob's effective value **and its source**; `--json` for the Hub; the writers write the scope's `limits:` block.
- §4: an unknown key or an unparsable duration is a load error naming path and line, not a silent default. `cost_usd` on a non-billable scope loads fine and is reported as "does not apply".
- §6: stale keys are loaded, reported, never an error; `loop.budget_usd` is read as `limits.cost_usd` when `limits:` is absent.
- **NEVER load-modify-save `config.yaml` through the typed `Config`** — it drops blocks other binaries own. Global writes are textual (`init.rs::append_fleet_run_if_absent` is the pattern).
- Changing a `mur-common` struct that the workspace-excluded Hub constructs literally breaks the Hub: after every struct change, `command grep -rn "<Type> {" mur-hub-gui/src-tauri/src` and fix literals; run the Hub check last.
- Before every commit: `cargo fmt`, `cargo clippy -p <crate> --all-targets -- -D warnings`, the named tests green. `std::os::unix::` in a fixture sits under `#[cfg(unix)]`.

## File structure

| File | Responsibility | Task |
|---|---|---|
| `mur-common/src/limits.rs` (new) | `Limits`, `Stuck`, `Source`, `Resolved`, `ResolvedLimits`, `parse_duration`, defaults, `resolve()`; tests | 1 |
| `mur-common/src/lib.rs` | `pub mod limits;` | 1 |
| `mur-common/src/config.rs` | `Config.limits: Limits` | 2 |
| `mur-common/src/fleet.rs` | `Fleet.limits: Option<Limits>`, `Fleet::limits_or_legacy()`; test | 2 |
| `mur-common/src/agent.rs` | `AgentProfile.limits: Option<Limits>` | 2 |
| `mur-core/src/cmd/fleet/loop_run.rs` | `parse_duration` delegates to `mur_common::limits::parse_duration` | 2 |
| `mur-core/src/cmd/limits.rs` (new) | scope detection, `LimitsReport`, `report()`, `print_human()`, `print_json()`, stale-key notes; tests | 3 |
| `mur-core/src/cmd/mod.rs`, `mur-core/src/cli/mod.rs`, `mur-core/src/dispatch.rs` | `mur limits <name> [--json]` | 3 |
| `mur-core/src/cmd/limits_write.rs` (new) | `write_fleet_limits`, `write_agent_limits`, `upsert_global_limits` (textual); tests | 4 |
| `mur-core/src/cli/actions.rs`, `mur-core/src/cli/agent.rs`, `mur-core/src/dispatch.rs` | `mur fleet limits`, `mur agent limits`, `mur limits --global …` | 4 |

---

## Task 1 — `mur_common::limits`: schema, grammar, defaults, resolver

**Interfaces.**
- Consumes: nothing from this plan.
- Produces (all `pub` in `mur_common::limits`):

```rust
pub struct Limits { pub deadline: Option<String>, pub stuck: Option<String>, pub cost_usd: Option<f64> }
pub enum Stuck { Off, After(Duration) }
pub enum Source { BuiltIn, Global, Fleet, Agent, Flag }
pub struct Resolved<T> { pub value: T, pub source: Source }
pub struct ResolvedLimits { pub deadline: Resolved<Option<Duration>>, pub stuck: Resolved<Stuck>, pub cost_usd: Resolved<Option<f64>> }
pub enum Scope { FleetRun, SingleTask }
pub const DEFAULT_STUCK: Duration            // 10 min
pub const DEFAULT_DEADLINE_FLEET: Duration   // 1 h  (unattended)
pub const DEFAULT_DEADLINE_TASK: Duration    // 30 min (unattended)
pub fn parse_duration(s: &str) -> Option<Duration>
pub fn validate(l: &Limits) -> Result<(), String>
pub fn resolve(scope: Scope, global: &Limits, fleet: Option<&Limits>, agent: Option<&Limits>, flags: &Limits) -> Result<ResolvedLimits, String>
```

### Steps

- [ ] **1.1 Create the module with its tests** — new file `mur-common/src/limits.rs`:

```rust
//! Execution limits: the one `limits:` block every scope carries (spec
//! 2026-09-12 execution-limits §3.1), its duration grammar, the built-in
//! defaults (§3.3) and the resolver that says what is in force and where it
//! came from (§3.9).
//!
//! Three knobs, no more: `deadline` (wall clock for the unit of work),
//! `stuck` (minutes of no progress before a stop, or `off`) and `cost_usd`
//! (only meaningful on a metered model — applicability is the caller's call,
//! this module resolves the number). Inner scopes REPLACE a key; nothing here
//! adds two caps together, because the product of caps is the problem the
//! spec exists to remove.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The block as written in YAML. Every key optional; an absent key means
/// "inherit", never "unlimited" — `stuck: off` is the explicit opt-out.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stuck: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

impl Limits {
    pub fn is_empty(&self) -> bool {
        self.deadline.is_none() && self.stuck.is_none() && self.cost_usd.is_none()
    }
}

/// The stuck detector's setting once resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stuck {
    Off,
    After(Duration),
}

/// Where a resolved value came from — the half of `mur limits` that makes it
/// worth running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    BuiltIn,
    Global,
    Fleet,
    Agent,
    Flag,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::BuiltIn => "built-in default",
            Source::Global => "~/.mur/config.yaml",
            Source::Fleet => "fleet.yaml",
            Source::Agent => "profile.yaml",
            Source::Flag => "command-line flag",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Resolved<T> {
    pub value: T,
    pub source: Source,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedLimits {
    pub deadline: Resolved<Option<Duration>>,
    pub stuck: Resolved<Stuck>,
    pub cost_usd: Resolved<Option<f64>>,
}

/// Which built-in deadline applies when no scope sets one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    FleetRun,
    SingleTask,
}

/// §3.3 — the constants `mur limits` prints as `(built-in default)`.
pub const DEFAULT_STUCK: Duration = Duration::from_secs(10 * 60);
pub const DEFAULT_DEADLINE_FLEET: Duration = Duration::from_secs(60 * 60);
pub const DEFAULT_DEADLINE_TASK: Duration = Duration::from_secs(30 * 60);

/// `30s`, `5m`, `2h`, `1d`, `1h30m`, or a bare integer (seconds). `None` on
/// anything else — the caller turns that into a load error naming the key.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(n) = s.parse::<u64>() {
        return Some(Duration::from_secs(n));
    }
    let mut total: u64 = 0;
    let mut num = String::new();
    let mut saw_unit = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let n: u64 = num.parse().ok()?;
        num.clear();
        let mult = match c {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            _ => return None,
        };
        total = total.checked_add(n.checked_mul(mult)?)?;
        saw_unit = true;
    }
    if !num.is_empty() || !saw_unit {
        return None;
    }
    Some(Duration::from_secs(total))
}

fn parse_stuck(s: &str) -> Option<Stuck> {
    if s.trim().eq_ignore_ascii_case("off") {
        return Some(Stuck::Off);
    }
    parse_duration(s).map(Stuck::After)
}

/// §4: an unparsable value is an error naming the key, never a silent default.
pub fn validate(l: &Limits) -> Result<(), String> {
    if let Some(d) = &l.deadline
        && parse_duration(d).is_none()
    {
        return Err(format!("limits.deadline: `{d}` is not a duration (30s, 5m, 2h, 1h30m, 1d)"));
    }
    if let Some(s) = &l.stuck
        && parse_stuck(s).is_none()
    {
        return Err(format!("limits.stuck: `{s}` is not a duration or `off`"));
    }
    if let Some(c) = l.cost_usd
        && !(c.is_finite() && c >= 0.0)
    {
        return Err(format!("limits.cost_usd: `{c}` must be a non-negative number"));
    }
    Ok(())
}

/// The resolver. Precedence, narrowest wins: flag > agent > fleet > global >
/// built-in. Each key is resolved on its own; a scope that sets only `stuck`
/// leaves `deadline` to the next scope out.
pub fn resolve(
    scope: Scope,
    global: &Limits,
    fleet: Option<&Limits>,
    agent: Option<&Limits>,
    flags: &Limits,
) -> Result<ResolvedLimits, String> {
    for (l, who) in [
        (Some(flags), "flag"),
        (agent, "profile.yaml"),
        (fleet, "fleet.yaml"),
        (Some(global), "config.yaml"),
    ] {
        if let Some(l) = l {
            validate(l).map_err(|e| format!("{who}: {e}"))?;
        }
    }
    // Narrowest first; the first scope that carries the key wins.
    let layers: [(Option<&Limits>, Source); 4] = [
        (Some(flags), Source::Flag),
        (agent, Source::Agent),
        (fleet, Source::Fleet),
        (Some(global), Source::Global),
    ];
    let pick = |get: &dyn Fn(&Limits) -> bool| -> Option<(&Limits, Source)> {
        layers
            .iter()
            .find_map(|(l, src)| l.filter(|l| get(l)).map(|l| (l, *src)))
    };

    let deadline = match pick(&|l| l.deadline.is_some()) {
        Some((l, src)) => Resolved {
            value: l.deadline.as_deref().and_then(parse_duration),
            source: src,
        },
        None => Resolved {
            value: Some(match scope {
                Scope::FleetRun => DEFAULT_DEADLINE_FLEET,
                Scope::SingleTask => DEFAULT_DEADLINE_TASK,
            }),
            source: Source::BuiltIn,
        },
    };
    let stuck = match pick(&|l| l.stuck.is_some()) {
        Some((l, src)) => Resolved {
            value: l.stuck.as_deref().and_then(parse_stuck).unwrap_or(Stuck::After(DEFAULT_STUCK)),
            source: src,
        },
        None => Resolved {
            value: Stuck::After(DEFAULT_STUCK),
            source: Source::BuiltIn,
        },
    };
    let cost_usd = match pick(&|l| l.cost_usd.is_some()) {
        Some((l, src)) => Resolved {
            value: l.cost_usd,
            source: src,
        },
        None => Resolved {
            value: None,
            source: Source::BuiltIn,
        },
    };
    Ok(ResolvedLimits {
        deadline,
        stuck,
        cost_usd,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(deadline: Option<&str>, stuck: Option<&str>, cost: Option<f64>) -> Limits {
        Limits {
            deadline: deadline.map(str::to_string),
            stuck: stuck.map(str::to_string),
            cost_usd: cost,
        }
    }

    #[test]
    fn durations_parse_the_spec_grammar_and_nothing_else() {
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
        assert_eq!(parse_duration("2h"), Some(Duration::from_secs(7200)));
        assert_eq!(parse_duration("1d"), Some(Duration::from_secs(86_400)));
        assert_eq!(parse_duration("1h30m"), Some(Duration::from_secs(5400)));
        assert_eq!(parse_duration(" 90 "), Some(Duration::from_secs(90)));
        for bad in ["", "off", "2 hours", "1h3", "h", "-5m", "1.5h"] {
            assert_eq!(parse_duration(bad), None, "{bad:?} must not parse");
        }
    }

    #[test]
    fn narrowest_scope_wins_per_key_and_never_combines() {
        let global = l(Some("4h"), Some("20m"), Some(50.0));
        let fleet = l(Some("2h"), None, None);
        let agent = l(None, Some("off"), None);
        let r = resolve(Scope::FleetRun, &global, Some(&fleet), Some(&agent), &Limits::default()).unwrap();
        assert_eq!(r.deadline.value, Some(Duration::from_secs(7200)));
        assert_eq!(r.deadline.source, Source::Fleet, "fleet set it, agent did not");
        assert_eq!(r.stuck.value, Stuck::Off);
        assert_eq!(r.stuck.source, Source::Agent);
        assert_eq!(r.cost_usd.value, Some(50.0));
        assert_eq!(r.cost_usd.source, Source::Global, "nobody narrower set it");

        // A flag beats everyone, for its key only.
        let flags = l(Some("10m"), None, None);
        let r = resolve(Scope::FleetRun, &global, Some(&fleet), Some(&agent), &flags).unwrap();
        assert_eq!(r.deadline.source, Source::Flag);
        assert_eq!(r.stuck.source, Source::Agent);
    }

    #[test]
    fn built_in_defaults_fill_what_no_scope_set_and_say_so() {
        let none = Limits::default();
        let r = resolve(Scope::FleetRun, &none, None, None, &none).unwrap();
        assert_eq!(r.deadline.value, Some(DEFAULT_DEADLINE_FLEET));
        assert_eq!(r.deadline.source, Source::BuiltIn);
        assert_eq!(r.stuck.value, Stuck::After(DEFAULT_STUCK));
        assert_eq!(r.cost_usd.value, None);
        let r = resolve(Scope::SingleTask, &none, None, None, &none).unwrap();
        assert_eq!(r.deadline.value, Some(DEFAULT_DEADLINE_TASK));
    }

    #[test]
    fn a_bad_value_is_an_error_that_names_the_key_and_the_scope() {
        let bad = l(Some("soon"), None, None);
        let e = resolve(Scope::FleetRun, &Limits::default(), Some(&bad), None, &Limits::default()).unwrap_err();
        assert!(e.contains("fleet.yaml") && e.contains("limits.deadline") && e.contains("soon"), "{e}");
        let e = validate(&l(None, Some("sometimes"), None)).unwrap_err();
        assert!(e.contains("limits.stuck"), "{e}");
        let e = validate(&l(None, None, Some(-1.0))).unwrap_err();
        assert!(e.contains("limits.cost_usd"), "{e}");
    }

    #[test]
    fn unknown_keys_are_rejected_at_load() {
        let e = serde_yaml_ng::from_str::<Limits>("deadline: 1h\nmax_iterations: 5\n").unwrap_err();
        assert!(e.to_string().contains("max_iterations"), "{e}");
        let ok: Limits = serde_yaml_ng::from_str("stuck: off\n").unwrap();
        assert_eq!(ok.stuck.as_deref(), Some("off"));
        assert!(serde_yaml_ng::to_string(&Limits::default()).unwrap().trim() == "{}");
    }
}
```

  If `mur-common` uses `serde_yaml` rather than `serde_yaml_ng` in its tests, use the same crate name the existing `model.rs` tests use.

- [ ] **1.2 Register** — in `mur-common/src/lib.rs`, between `pub mod ledger;` and `pub mod llm;` add `pub mod limits;`.

- [ ] **1.3 Watch it pass** — `cargo nextest run -p mur-common --lib -E 'test(/limits::/)'`. Expected: 5 pass. (The red for this task is the missing module before 1.2.) Then `cargo clippy -p mur-common --all-targets -- -D warnings`, `cargo fmt -p mur-common`.

- [ ] **1.4 Commit**:

```
feat(limits): the limits: schema, its grammar, defaults and resolver

Three knobs — deadline, stuck, cost_usd — one shape at every scope, and a
resolver that reports value AND source per key. Narrowest scope wins per
key; nothing combines two caps. Unknown keys and unparsable durations are
load errors that name the key and the scope.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 2 — the block on the three scopes, and the legacy read

**Interfaces.**
- Consumes: `mur_common::limits::{Limits, parse_duration}` (Task 1).
- Produces: `Config.limits: Limits`; `Fleet.limits: Option<Limits>`; `AgentProfile.limits: Option<Limits>`; `Fleet::limits_or_legacy(&self) -> Option<Limits>` (§6: `loop.deadline` / `loop.budget_usd` read as `limits` when `limits:` is absent); `mur_core::cmd::fleet::loop_run::parse_duration` delegates to the common one.

### Steps

- [ ] **2.1 Write the failing test** — in `mur-common/src/fleet.rs`'s test module (create `#[cfg(test)] mod limits_tests` at the end of the file if there is none):

```rust
#[cfg(test)]
mod limits_tests {
    use super::*;

    fn fleet(loop_cfg: Option<FleetLoop>, limits: Option<crate::limits::Limits>) -> Fleet {
        Fleet {
            name: "dev".into(),
            display_name: String::new(),
            goal: "g".into(),
            router: None,
            team_id: None,
            members: vec![],
            channel_id: "fleet-dev".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg,
            parallel: None,
            hitl: None,
            requires_programs: vec![],
            limits,
        }
    }

    /// §6: a fleet written before `limits:` existed keeps its bound — the old
    /// loop.deadline / loop.budget_usd are read as limits until it is
    /// rewritten. An explicit `limits:` block wins outright, even if empty.
    #[test]
    fn legacy_loop_fields_are_read_as_limits_only_when_the_block_is_absent() {
        let lc = FleetLoop {
            trigger: "manual".into(),
            max_iterations: 8,
            budget_usd: 5.0,
            deadline: "2h".into(),
            done_when: String::new(),
        };
        let legacy = fleet(Some(lc.clone()), None).limits_or_legacy().expect("derived");
        assert_eq!(legacy.deadline.as_deref(), Some("2h"));
        assert_eq!(legacy.cost_usd, Some(5.0));
        assert_eq!(legacy.stuck, None, "the old loop had no stuck setting");

        // Zero / empty legacy values are absence, not a value.
        let zero = FleetLoop { budget_usd: 0.0, deadline: String::new(), ..lc.clone() };
        assert_eq!(fleet(Some(zero), None).limits_or_legacy(), None);

        // An explicit block, even empty, is authoritative.
        let explicit = fleet(Some(lc), Some(crate::limits::Limits::default())).limits_or_legacy();
        assert_eq!(explicit, Some(crate::limits::Limits::default()));

        // Round trip: a fleet without limits serialises without the key.
        let yaml = serde_yaml_ng::to_string(&fleet(None, None)).unwrap();
        assert!(!yaml.contains("limits"), "{yaml}");
    }
}
```

- [ ] **2.2 Watch it fail** — `cargo nextest run -p mur-common --lib -E 'test(legacy_loop_fields_are_read)'`. Expected: `struct Fleet has no field named limits`.

- [ ] **2.3 Add the field to each scope** — 

  `mur-common/src/fleet.rs`, after `pub requires_programs: Vec<ProgramDep>,`:

```rust
    /// Execution limits for runs of this fleet (spec 2026-09-12 §3.1). Absent
    /// → inherit from `config.yaml`; see `limits_or_legacy` for the read of
    /// pre-`limits:` files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<crate::limits::Limits>,
```

  and in `impl Fleet` (next to `router_or_concierge`):

```rust
    /// The fleet's `limits:` block, or one derived from the legacy
    /// `loop.deadline` / `loop.budget_usd` when the block is absent (§6). An
    /// explicit block — even an empty one — is authoritative; legacy zero /
    /// empty values are absence, not a value.
    pub fn limits_or_legacy(&self) -> Option<crate::limits::Limits> {
        if let Some(l) = &self.limits {
            return Some(l.clone());
        }
        let lc = self.loop_cfg.as_ref()?;
        let deadline = (!lc.deadline.trim().is_empty()).then(|| lc.deadline.trim().to_string());
        let cost_usd = (lc.budget_usd > 0.0).then_some(lc.budget_usd);
        if deadline.is_none() && cost_usd.is_none() {
            return None;
        }
        Some(crate::limits::Limits {
            deadline,
            stuck: None,
            cost_usd,
        })
    }
```

  `mur-common/src/config.rs`, after `pub capture: CaptureConfig,`:

```rust
    /// Global execution limits (`limits:`), the outermost scope of spec
    /// 2026-09-12 §3.1. Written textually by `mur limits --global`, never by
    /// load-modify-save (that drops blocks other binaries own).
    #[serde(default)]
    pub limits: crate::limits::Limits,
```

  `mur-common/src/agent.rs`, after `pub hitl: HitlConfig,`:

```rust
    /// Execution limits for this agent's own tasks (spec 2026-09-12 §3.1).
    /// Absent → inherit. Replaces `hitl.max_iterations` / `hitl.max_tokens`,
    /// which stay readable for the migration warning until the runtime
    /// switch (step 4) stops applying them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<crate::limits::Limits>,
```

- [ ] **2.4 Fix every struct literal** — `command grep -rn "Fleet {\|AgentProfile {\|Config {" mur-common/src mur-core/src mur-daemon/src mur-agent-runtime/src mur-mcp-server/src mur-hub-gui/src-tauri/src mur-gui-core/src | command grep -v "FleetLoop\|FleetHitl\|FleetBilling\|FleetSummary\|FleetDetail\|EmbeddingConfig\|LlmConfig\|HitlConfig\|ConfigFile\|Config::"` and add `limits: None,` (Fleet, AgentProfile) or rely on `..Default::default()` where present. `Config` derives `Default`, so its literals with `..Default::default()` need nothing; literals without it get `limits: Default::default(),`. The Hub is workspace-excluded — its literals are found by this grep but only compiled by `cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml`; run that **last** if `mur-hub-gui/ui/dist` exists, and record the result in the commit body either way.

- [ ] **2.5 Delegate the loop's duration grammar** — in `mur-core/src/cmd/fleet/loop_run.rs`, replace the body of `pub fn parse_duration(s: &str) -> Option<Duration>` with:

```rust
pub fn parse_duration(s: &str) -> Option<Duration> {
    // One grammar for `--deadline`, `loop.deadline` and `limits.deadline`.
    mur_common::limits::parse_duration(s)
}
```

  and delete the private helpers it used if they are now unused (clippy will name them).

- [ ] **2.6 Watch it pass** — `cargo nextest run -p mur-common --lib -E 'test(/limits/)'` then `cargo nextest run -p mur-core --lib -E 'test(/loop_run::|parse_duration/)'`. Expected: all pass, including the existing `parse_duration` tests in `loop_run.rs` (the grammar is a superset: `1h30m` now parses; if a test asserted it did **not**, update that assertion and say so in the commit).

- [ ] **2.7 fmt + clippy on `mur-common`, `mur-core`, `mur-daemon`, `mur-agent-runtime`**, then **commit**:

```
feat(limits): the limits: block on config.yaml, fleet.yaml and profile.yaml

Same shape at every scope, absent means inherit. A fleet written before
the block existed keeps its bound: loop.deadline / loop.budget_usd are read
as limits until the file is rewritten (§6). The loop's --deadline grammar
now comes from mur_common::limits so a flag, a fleet.yaml and a limits.yaml
cannot disagree about what "1h30m" means.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 3 — `mur limits <name> [--json]`: what is in force, and from where

**Interfaces.**
- Consumes: `mur_common::limits::{resolve, Scope, Source, Stuck, Limits}` (Task 1), the three fields + `limits_or_legacy` (Task 2), `mur_core::cmd::fleet::billing::{fleet_billing, FleetBilling}` (step 2), `AgentProfile::load(mur_home, name)`, `store::load_fleet(mur_home, name)`, `Config::load_or_default(&home.join("config.yaml"))`, `ModelRegistry::load_from`, `ModelEntry::billing_or_inferred`.
- Produces (in `mur-core/src/cmd/limits.rs`):

```rust
pub enum Target { Fleet(String), Agent(String) }
pub struct Row { pub knob: &'static str, pub value: String, pub source: String, pub note: Option<String> }
pub struct LimitsReport { pub target: Target, pub rows: Vec<Row>, pub stale: Vec<String>, pub attended_note: &'static str }
pub fn detect_target(mur_home: &Path, name: &str) -> anyhow::Result<Target>
pub fn report(mur_home: &Path, target: &Target) -> anyhow::Result<LimitsReport>
pub fn render_human(r: &LimitsReport) -> String
pub fn render_json(r: &LimitsReport) -> serde_json::Value
pub fn cmd_limits(name: &str, json: bool) -> anyhow::Result<()>
```

### Steps

- [ ] **3.1 Write the failing tests** — new file `mur-core/src/cmd/limits.rs` starting with the tests (the module body comes in 3.3; put the tests at the bottom of the same file):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::fleet::{Fleet, FleetLoop};

    fn home_with_fleet(fleet: &Fleet) -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        crate::cmd::fleet::store::save_fleet(t.path(), fleet).unwrap();
        t
    }

    fn dev(loop_cfg: Option<FleetLoop>, limits: Option<mur_common::limits::Limits>) -> Fleet {
        Fleet {
            name: "dev".into(),
            display_name: String::new(),
            goal: "g".into(),
            router: None,
            team_id: None,
            members: vec!["pm".into()],
            channel_id: "fleet-dev".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg,
            parallel: None,
            hitl: None,
            requires_programs: vec![],
            limits,
        }
    }

    /// The report says, per knob, the value AND where it came from — the
    /// whole point of the command. With no models.yaml the fleet's billing is
    /// unknown → billable, so cost_usd applies and shows as unset.
    #[test]
    fn a_fleet_report_names_each_source() {
        let f = dev(
            None,
            Some(mur_common::limits::Limits {
                deadline: Some("2h".into()),
                stuck: None,
                cost_usd: None,
            }),
        );
        let t = home_with_fleet(&f);
        let r = report(t.path(), &Target::Fleet("dev".into())).unwrap();
        let by = |k: &str| r.rows.iter().find(|row| row.knob == k).unwrap();
        assert_eq!(by("deadline").value, "2h");
        assert_eq!(by("deadline").source, "fleet.yaml");
        assert_eq!(by("stuck").value, "10m");
        assert_eq!(by("stuck").source, "built-in default");
        assert_eq!(by("cost_usd").value, "—");
        assert!(by("cost_usd").note.as_deref().unwrap_or("").contains("billing unknown"), "{:?}", by("cost_usd").note);
        assert!(r.stale.is_empty());
        let text = render_human(&r);
        assert!(text.contains("deadline") && text.contains("← fleet.yaml"), "{text}");
    }

    /// §6: a pre-`limits:` fleet is reported from its legacy fields, labelled
    /// legacy, and its max_iterations is listed as stale with the step-4 note.
    #[test]
    fn a_legacy_fleet_is_read_and_its_stale_keys_named() {
        let f = dev(
            Some(FleetLoop {
                trigger: "manual".into(),
                max_iterations: 8,
                budget_usd: 5.0,
                deadline: "2h".into(),
                done_when: String::new(),
            }),
            None,
        );
        let t = home_with_fleet(&f);
        let r = report(t.path(), &Target::Fleet("dev".into())).unwrap();
        let by = |k: &str| r.rows.iter().find(|row| row.knob == k).unwrap();
        assert_eq!(by("deadline").source, "fleet.yaml (legacy loop.deadline)");
        assert_eq!(by("cost_usd").value, "$5.00");
        assert!(r.stale.iter().any(|s| s.contains("loop.max_iterations: 8")), "{:?}", r.stale);
        assert!(r.stale[0].contains("still applied until"), "honest about step 4: {}", r.stale[0]);
    }

    /// Unknown name → a clear error, not an empty report.
    #[test]
    fn an_unknown_name_is_an_error_naming_both_kinds() {
        let t = tempfile::tempdir().unwrap();
        let e = detect_target(t.path(), "nobody").unwrap_err().to_string();
        assert!(e.contains("fleet") && e.contains("agent"), "{e}");
    }

    #[test]
    fn json_carries_rows_and_stale_keys() {
        let f = dev(None, None);
        let t = home_with_fleet(&f);
        let r = report(t.path(), &Target::Fleet("dev".into())).unwrap();
        let j = render_json(&r);
        assert_eq!(j["target"]["kind"], "fleet");
        assert_eq!(j["rows"].as_array().unwrap().len(), 3);
        assert!(j["stale"].is_array());
    }
}
```

- [ ] **3.2 Watch it fail** — add `pub mod limits;` to `mur-core/src/cmd/mod.rs` (alphabetical, next to the other `pub mod` lines), then `cargo nextest run -p mur-core --lib -E 'test(/cmd::limits::/)'`. Expected: compile errors `cannot find function report` etc.

- [ ] **3.3 Write the module** — above the tests in `mur-core/src/cmd/limits.rs`:

```rust
//! `mur limits <name>` — every execution knob in force for a fleet or an
//! agent, each with the scope it came from (spec 2026-09-12 §3.9), plus the
//! legacy keys that will stop applying at the runtime switch (§6).

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use mur_common::limits::{Limits, Resolved, ResolvedLimits, Scope, Source, Stuck, resolve};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Fleet(String),
    Agent(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub knob: &'static str,
    pub value: String,
    pub source: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LimitsReport {
    pub target: Target,
    pub rows: Vec<Row>,
    /// Legacy keys still present in the files, each with what happens to it.
    pub stale: Vec<String>,
    pub attended_note: &'static str,
}

/// A name is a fleet if `fleets/<name>/fleet.yaml` loads, else an agent if
/// `agents/<name>/profile.yaml` loads. Both missing is an error that says so
/// — an empty report for a typo would be worse than none.
pub fn detect_target(mur_home: &Path, name: &str) -> Result<Target> {
    if crate::cmd::fleet::store::load_fleet(mur_home, name).is_ok() {
        return Ok(Target::Fleet(name.to_string()));
    }
    if mur_common::agent::AgentProfile::load(mur_home, name).is_ok() {
        return Ok(Target::Agent(name.to_string()));
    }
    Err(anyhow!(
        "`{name}` is neither a fleet (~/.mur/fleets/{name}/fleet.yaml) nor an agent (~/.mur/agents/{name}/profile.yaml)"
    ))
}

const STEP4_NOTE: &str =
    "still applied until the runtime switch (execution-limits step 4); ignored after — move it to limits:";

fn fmt_dur(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s % 3600 == 0 {
        format!("{}h", s / 3600)
    } else if s % 60 == 0 {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

fn source_label(src: Source, legacy_deadline: bool, legacy_cost: bool, knob: &str) -> String {
    match (src, knob) {
        (Source::Fleet, "deadline") if legacy_deadline => "fleet.yaml (legacy loop.deadline)".into(),
        (Source::Fleet, "cost_usd") if legacy_cost => "fleet.yaml (legacy loop.budget_usd)".into(),
        (s, _) => s.label().to_string(),
    }
}

fn rows_from(
    r: &ResolvedLimits,
    legacy_deadline: bool,
    legacy_cost: bool,
    cost_applies: Option<String>, // None = applies; Some(why) = does not
) -> Vec<Row> {
    let deadline = Row {
        knob: "deadline",
        value: match r.deadline.value {
            Some(d) => fmt_dur(d),
            None => "—".into(),
        },
        source: source_label(r.deadline.source, legacy_deadline, false, "deadline"),
        note: None,
    };
    let stuck = Row {
        knob: "stuck",
        value: match r.stuck.value {
            Stuck::Off => "off".into(),
            Stuck::After(d) => fmt_dur(d),
        },
        source: r.stuck.source.label().to_string(),
        note: None,
    };
    let cost = match cost_applies {
        None => Row {
            knob: "cost_usd",
            value: match r.cost_usd.value {
                Some(c) => format!("${c:.2}"),
                None => "—".into(),
            },
            source: source_label(r.cost_usd.source, false, legacy_cost, "cost_usd"),
            note: None,
        },
        Some(why) => Row {
            knob: "cost_usd",
            value: "—".into(),
            source: String::new(),
            note: Some(why),
        },
    };
    vec![deadline, stuck, cost]
}

pub fn report(mur_home: &Path, target: &Target) -> Result<LimitsReport> {
    let cfg = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml"));
    let global = cfg.limits.clone();
    let none = Limits::default();
    match target {
        Target::Fleet(name) => {
            let fleet = crate::cmd::fleet::store::load_fleet(mur_home, name)?;
            let explicit = fleet.limits.is_some();
            let fl = fleet.limits_or_legacy();
            let legacy_deadline = !explicit && fl.as_ref().is_some_and(|l| l.deadline.is_some());
            let legacy_cost = !explicit && fl.as_ref().is_some_and(|l| l.cost_usd.is_some());
            let resolved = resolve(Scope::FleetRun, &global, fl.as_ref(), None, &none)
                .map_err(|e| anyhow!("{e}"))?;
            let billing = crate::cmd::fleet::billing::fleet_billing(mur_home, &fleet);
            let cost_applies = if billing.billable {
                if billing.unknown.is_empty() {
                    None
                } else {
                    // Applies, but say why: unknown counts as metered.
                    None
                }
            } else {
                Some("runs on local/subscription models — a cost cap does not apply".into())
            };
            let mut rows = rows_from(&resolved, legacy_deadline, legacy_cost, cost_applies);
            if !billing.unknown.is_empty() {
                let who: Vec<String> = billing.unknown.iter().map(|(a, m)| format!("{a} ({m})")).collect();
                rows[2].note = Some(format!(
                    "billing unknown for {} — treated as metered; mark a local model with `billing: local` in models.yaml",
                    who.join(", ")
                ));
            }
            let mut stale = Vec::new();
            if let Some(lc) = &fleet.loop_cfg {
                if lc.max_iterations != 0 {
                    stale.push(format!("loop.max_iterations: {} in fleet.yaml — {STEP4_NOTE}", lc.max_iterations));
                }
                if explicit && lc.budget_usd > 0.0 {
                    stale.push(format!("loop.budget_usd: {} in fleet.yaml — shadowed by limits: (remove it)", lc.budget_usd));
                }
                if explicit && !lc.deadline.trim().is_empty() {
                    stale.push(format!("loop.deadline: {} in fleet.yaml — shadowed by limits: (remove it)", lc.deadline.trim()));
                }
            }
            Ok(LimitsReport {
                target: target.clone(),
                rows,
                stale,
                attended_note: "attended runs (murmur) have no hard stops; stuck only warns",
            })
        }
        Target::Agent(name) => {
            let profile = mur_common::agent::AgentProfile::load(mur_home, name)
                .with_context(|| format!("load agent {name}"))?;
            let resolved = resolve(Scope::SingleTask, &global, None, profile.limits.as_ref(), &none)
                .map_err(|e| anyhow!("{e}"))?;
            let registry = mur_common::model::ModelRegistry::load_from(&mur_home.join("models.yaml")).ok();
            let billing = profile
                .model_ref
                .as_ref()
                .and_then(|r| registry.as_ref()?.models.get(r))
                .map(|e| e.billing_or_inferred());
            let cost_applies = match billing {
                Some(mur_common::model::BillingMode::UsageBilled) => None,
                Some(mur_common::model::BillingMode::Local) => Some("model is local — a cost cap does not apply".into()),
                Some(mur_common::model::BillingMode::Subscription) => Some("model is subscription-billed — a cost cap does not apply".into()),
                None => None,
            };
            let mut rows = rows_from(&resolved, false, false, cost_applies);
            if billing.is_none() {
                rows[2].note = Some("billing unknown — treated as metered; mark a local model with `billing: local` in models.yaml".into());
            }
            let mut stale = Vec::new();
            if let Some(n) = profile.hitl.max_iterations {
                stale.push(format!("hitl.max_iterations: {n} in profile.yaml — {STEP4_NOTE}"));
            }
            if let Some(n) = profile.hitl.max_tokens {
                stale.push(format!("hitl.max_tokens: {n} in profile.yaml — {STEP4_NOTE}"));
            }
            Ok(LimitsReport {
                target: target.clone(),
                rows,
                stale,
                attended_note: "attended turns (murmur, Hub chat) have no hard stops; stuck only warns",
            })
        }
    }
}

pub fn render_human(r: &LimitsReport) -> String {
    let mut out = String::new();
    match &r.target {
        Target::Fleet(n) => out.push_str(&format!("scope: fleet {n}\n")),
        Target::Agent(n) => out.push_str(&format!("scope: agent {n}\n")),
    }
    for row in &r.rows {
        if row.source.is_empty() {
            out.push_str(&format!("{:<10} {:<8} {}\n", row.knob, row.value, row.note.clone().unwrap_or_default()));
        } else {
            out.push_str(&format!("{:<10} {:<8} ← {}\n", row.knob, row.value, row.source));
            if let Some(n) = &row.note {
                out.push_str(&format!("           {n}\n"));
            }
        }
    }
    out.push_str(&format!("note: {}\n", r.attended_note));
    for s in &r.stale {
        out.push_str(&format!("stale: {s}\n"));
    }
    out
}

pub fn render_json(r: &LimitsReport) -> serde_json::Value {
    let (kind, name) = match &r.target {
        Target::Fleet(n) => ("fleet", n),
        Target::Agent(n) => ("agent", n),
    };
    serde_json::json!({
        "target": {"kind": kind, "name": name},
        "rows": r.rows.iter().map(|row| serde_json::json!({
            "knob": row.knob, "value": row.value, "source": row.source, "note": row.note,
        })).collect::<Vec<_>>(),
        "stale": r.stale,
        "attended_note": r.attended_note,
    })
}

pub fn cmd_limits(name: &str, json: bool) -> Result<()> {
    let home = crate::cmd::resolve_mur_home()?;
    let target = detect_target(&home, name)?;
    let r = report(&home, &target)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&render_json(&r))?);
    } else {
        print!("{}", render_human(&r));
    }
    Ok(())
}
```

  Unused-import cleanup: `Resolved` may be unused — drop it from the `use` if clippy says so.

- [ ] **3.4 Wire the command** — `mur-core/src/cli/mod.rs`, in `Commands`, next to `Doctor`:

```rust
    /// Every execution limit in force for a fleet or an agent, with the scope
    /// it came from, and the legacy keys that will stop applying.
    Limits {
        /// Fleet or agent name
        name: String,
        /// Emit JSON instead of the table
        #[arg(long)]
        json: bool,
    },
```

  `mur-core/src/dispatch.rs`, next to `Commands::Doctor =>`:

```rust
        Commands::Limits { name, json } => cmd::limits::cmd_limits(&name, json)?,
```

- [ ] **3.5 Watch it pass** — `cargo nextest run -p mur-core --lib -E 'test(/cmd::limits::/)'`. Expected: 4 pass. Then `cargo run -p mur-core --bin mur -- limits deep-research` against the real home prints three rows with sources (informational; do not assert values, they are this machine's).

- [ ] **3.6 fmt + clippy**, then **commit**:

```
feat(cli): mur limits <name> — every knob in force, with its source

Prints deadline / stuck / cost_usd for a fleet or an agent, each labelled
with the scope it came from (flag, profile.yaml, fleet.yaml, config.yaml,
built-in default), whether cost_usd even applies to the scope's billing,
and the legacy keys still present — honestly: they keep applying until the
runtime switch in step 4. --json for the Hub.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 4 — the writers: one scope each, config.yaml textually

**Interfaces.**
- Consumes: `Limits`, `validate` (Task 1); `Fleet.limits`, `AgentProfile.limits` (Task 2); `store::save_fleet`; `crate::cmd::agent::load_profile_for_edit(name) -> Result<(PathBuf, AgentProfile)>` and `crate::cmd::agent::save_profile(&path, &mut profile)`; `init.rs::append_fleet_run_if_absent` as the textual pattern.
- Produces (in `mur-core/src/cmd/limits_write.rs`):

```rust
pub struct Patch { pub deadline: Option<String>, pub stuck: Option<String>, pub cost_usd: Option<f64>, pub unset: Vec<String> }
pub fn apply_patch(current: Option<Limits>, patch: &Patch) -> Result<Option<Limits>, String>
pub fn write_fleet_limits(mur_home: &Path, name: &str, patch: &Patch) -> anyhow::Result<()>
pub fn write_agent_limits(name: &str, patch: &Patch) -> anyhow::Result<()>
pub fn upsert_global_limits(config_path: &Path, patch: &Patch) -> anyhow::Result<()>
pub fn render_limits_block(l: &Limits) -> String
```

### Steps

- [ ] **4.1 Write the failing tests** — new file `mur-core/src/cmd/limits_write.rs`, tests at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::limits::Limits;

    #[test]
    fn a_patch_sets_and_unsets_keys_and_validates() {
        let p = Patch { deadline: Some("2h".into()), stuck: None, cost_usd: Some(5.0), unset: vec![] };
        let l = apply_patch(None, &p).unwrap().unwrap();
        assert_eq!(l.deadline.as_deref(), Some("2h"));
        assert_eq!(l.cost_usd, Some(5.0));
        let p2 = Patch { deadline: None, stuck: Some("off".into()), cost_usd: None, unset: vec!["cost_usd".into()] };
        let l2 = apply_patch(Some(l), &p2).unwrap().unwrap();
        assert_eq!(l2.cost_usd, None);
        assert_eq!(l2.stuck.as_deref(), Some("off"));
        assert_eq!(l2.deadline.as_deref(), Some("2h"), "untouched keys survive");
        // Unsetting the last key leaves no block (None), so the scope inherits.
        let p3 = Patch { deadline: None, stuck: None, cost_usd: None, unset: vec!["deadline".into(), "stuck".into()] };
        assert_eq!(apply_patch(Some(l2), &p3).unwrap(), None);
        // Bad values are refused before anything is written.
        let bad = Patch { deadline: Some("soon".into()), stuck: None, cost_usd: None, unset: vec![] };
        assert!(apply_patch(None, &bad).unwrap_err().contains("limits.deadline"));
        let unknown = Patch { deadline: None, stuck: None, cost_usd: None, unset: vec!["max_iterations".into()] };
        assert!(apply_patch(None, &unknown).unwrap_err().contains("max_iterations"));
    }

    /// The global writer edits config.yaml as TEXT: blocks it does not own
    /// survive byte-for-byte, an existing limits: block is replaced in place,
    /// and a missing one is appended.
    #[test]
    fn global_write_is_textual_and_preserves_foreign_blocks() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("config.yaml");
        std::fs::write(&p, "research_gateway:\n  brave_api_key_ref: keychain:mur/brave\nfleet_run:\n  agents: [mur]\n").unwrap();
        upsert_global_limits(&p, &Patch { deadline: Some("4h".into()), stuck: None, cost_usd: None, unset: vec![] }).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("research_gateway:\n  brave_api_key_ref: keychain:mur/brave\n"), "{text}");
        assert!(text.contains("fleet_run:\n  agents: [mur]\n"), "{text}");
        assert!(text.contains("limits:\n  deadline: 4h\n"), "{text}");
        // Replace in place, not append twice.
        upsert_global_limits(&p, &Patch { deadline: None, stuck: Some("15m".into()), cost_usd: None, unset: vec![] }).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(text.matches("limits:").count(), 1, "{text}");
        assert!(text.contains("  deadline: 4h\n") && text.contains("  stuck: 15m\n"), "{text}");
        // The typed loader agrees with the text.
        let cfg = mur_common::config::Config::load_or_default(&p);
        assert_eq!(cfg.limits.deadline.as_deref(), Some("4h"));
        assert_eq!(cfg.limits.stuck.as_deref(), Some("15m"));
        // Unsetting every key removes the block entirely.
        upsert_global_limits(&p, &Patch { deadline: None, stuck: None, cost_usd: None, unset: vec!["deadline".into(), "stuck".into()] }).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(!text.contains("limits:"), "{text}");
        assert!(text.contains("research_gateway:"), "{text}");
    }

    #[test]
    fn render_block_is_stable_yaml() {
        let l = Limits { deadline: Some("2h".into()), stuck: Some("off".into()), cost_usd: Some(5.0) };
        assert_eq!(render_limits_block(&l), "limits:\n  deadline: 2h\n  stuck: off\n  cost_usd: 5\n");
        let l = Limits { deadline: None, stuck: None, cost_usd: Some(0.5) };
        assert_eq!(render_limits_block(&l), "limits:\n  cost_usd: 0.5\n");
    }
}
```

- [ ] **4.2 Watch it fail** — add `pub mod limits_write;` to `mur-core/src/cmd/mod.rs`; `cargo nextest run -p mur-core --lib -E 'test(/limits_write::/)'`. Expected: compile errors for the missing items.

- [ ] **4.3 Write the module** — above the tests:

```rust
//! Writers for the `limits:` block, one scope each (spec 2026-09-12 §3.9).
//! Fleet and agent go through their typed stores. The GLOBAL scope is
//! `config.yaml`, and that file is edited as text: the typed `Config` drops
//! blocks other binaries own (`research_gateway:` is the known casualty), so
//! load-modify-save there is a data-loss bug, not a shortcut.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use mur_common::limits::{Limits, validate};

/// What one command invocation changes. `unset` names keys to remove.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Patch {
    pub deadline: Option<String>,
    pub stuck: Option<String>,
    pub cost_usd: Option<f64>,
    pub unset: Vec<String>,
}

const KEYS: [&str; 3] = ["deadline", "stuck", "cost_usd"];

/// Pure: the block after the patch, validated. `Ok(None)` means "no keys
/// left — drop the block so the scope inherits".
pub fn apply_patch(current: Option<Limits>, patch: &Patch) -> Result<Option<Limits>, String> {
    let mut l = current.unwrap_or_default();
    for k in &patch.unset {
        match k.as_str() {
            "deadline" => l.deadline = None,
            "stuck" => l.stuck = None,
            "cost_usd" => l.cost_usd = None,
            other => return Err(format!("`{other}` is not a limits key (one of: {})", KEYS.join(", "))),
        }
    }
    if let Some(d) = &patch.deadline {
        l.deadline = Some(d.trim().to_string());
    }
    if let Some(s) = &patch.stuck {
        l.stuck = Some(s.trim().to_string());
    }
    if let Some(c) = patch.cost_usd {
        l.cost_usd = Some(c);
    }
    validate(&l)?;
    Ok((!l.is_empty()).then_some(l))
}

pub fn write_fleet_limits(mur_home: &Path, name: &str, patch: &Patch) -> Result<()> {
    let mut fleet = crate::cmd::fleet::store::load_fleet(mur_home, name)?;
    // A legacy fleet's first write materialises the derived block, so the
    // user edits what `mur limits` showed them, not a hidden loop.* field.
    let current = fleet.limits_or_legacy();
    fleet.limits = apply_patch(current, patch).map_err(|e| anyhow!("{e}"))?;
    crate::cmd::fleet::store::save_fleet(mur_home, &fleet)?;
    println!("limits for fleet '{name}' updated — takes effect on its next run");
    Ok(())
}

pub fn write_agent_limits(name: &str, patch: &Patch) -> Result<()> {
    let (path, mut profile) = crate::cmd::agent::load_profile_for_edit(name)?;
    profile.limits = apply_patch(profile.limits.take(), patch).map_err(|e| anyhow!("{e}"))?;
    crate::cmd::agent::save_profile(&path, &mut profile)?;
    println!("limits for agent '{name}' updated — restart the agent to apply (mur agent restart {name})");
    Ok(())
}

/// YAML for the block, keys in schema order, numbers without a trailing `.0`.
pub fn render_limits_block(l: &Limits) -> String {
    let mut out = String::from("limits:\n");
    if let Some(d) = &l.deadline {
        out.push_str(&format!("  deadline: {d}\n"));
    }
    if let Some(s) = &l.stuck {
        out.push_str(&format!("  stuck: {s}\n"));
    }
    if let Some(c) = l.cost_usd {
        let c = if c.fract() == 0.0 { format!("{}", c as i64) } else { format!("{c}") };
        out.push_str(&format!("  cost_usd: {c}\n"));
    }
    out
}

/// Textual upsert of the top-level `limits:` block in `config.yaml`. The
/// block is the lines from `limits:` up to the next top-level key (a line
/// that starts in column 0 and is not blank or a comment). Everything else is
/// preserved byte-for-byte.
pub fn upsert_global_limits(config_path: &Path, patch: &Patch) -> Result<()> {
    let text = std::fs::read_to_string(config_path).unwrap_or_default();
    let current = mur_common::config::Config::load_or_default(config_path).limits;
    let next = apply_patch((!current.is_empty()).then_some(current), patch).map_err(|e| anyhow!("{e}"))?;

    let lines: Vec<&str> = text.lines().collect();
    let start = lines.iter().position(|l| l.trim_end() == "limits:" || l.starts_with("limits:"));
    let (before, after): (Vec<&str>, Vec<&str>) = match start {
        Some(s) => {
            let mut e = s + 1;
            while e < lines.len() {
                let l = lines[e];
                let top_level = !l.is_empty() && !l.starts_with(' ') && !l.starts_with('\t') && !l.starts_with('#');
                if top_level {
                    break;
                }
                e += 1;
            }
            (lines[..s].to_vec(), lines[e..].to_vec())
        }
        None => (lines.clone(), Vec::new()),
    };
    let mut out = before.join("\n");
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if let Some(l) = &next {
        out.push_str(&render_limits_block(l));
    }
    if !after.is_empty() {
        out.push_str(&after.join("\n"));
        out.push('\n');
    }
    let tmp = config_path.with_extension("yaml.tmp");
    std::fs::write(&tmp, out).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, config_path).with_context(|| format!("rename to {}", config_path.display()))?;
    println!("global limits updated in {}", config_path.display());
    Ok(())
}
```

- [ ] **4.4 Wire three commands** —

  `mur-core/src/cli/mod.rs`: extend `Commands::Limits`:

```rust
    Limits {
        /// Fleet or agent name (omit with --global)
        name: Option<String>,
        #[arg(long)]
        json: bool,
        /// Edit ~/.mur/config.yaml's limits: block instead of a fleet or agent
        #[arg(long)]
        global: bool,
        /// Wall clock for the unit of work, e.g. 30m / 2h / 1h30m
        #[arg(long)]
        deadline: Option<String>,
        /// Minutes of no progress before stopping (unattended) or warning (attended); `off` disables
        #[arg(long)]
        stuck: Option<String>,
        /// Cost cap in USD — applies only to metered models
        #[arg(long)]
        cost_usd: Option<f64>,
        /// Remove a key so the scope inherits it (repeatable)
        #[arg(long = "unset")]
        unset: Vec<String>,
    },
```

  `mur-core/src/dispatch.rs`:

```rust
        Commands::Limits { name, json, global, deadline, stuck, cost_usd, unset } => {
            let patch = cmd::limits_write::Patch { deadline, stuck, cost_usd, unset };
            let editing = patch != cmd::limits_write::Patch::default();
            let home = cmd::resolve_mur_home()?;
            match (global, name) {
                (true, _) if editing => cmd::limits_write::upsert_global_limits(&home.join("config.yaml"), &patch)?,
                (true, _) => {
                    let cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
                    print!("{}", cmd::limits_write::render_limits_block(&cfg.limits));
                }
                (false, Some(n)) if editing => match cmd::limits::detect_target(&home, &n)? {
                    cmd::limits::Target::Fleet(f) => cmd::limits_write::write_fleet_limits(&home, &f, &patch)?,
                    cmd::limits::Target::Agent(a) => cmd::limits_write::write_agent_limits(&a, &patch)?,
                },
                (false, Some(n)) => cmd::limits::cmd_limits(&n, json)?,
                (false, None) => anyhow::bail!("give a fleet or agent name, or --global"),
            }
        }
```

  and update Task 3's `Commands::Limits` shape accordingly (this replaces the 3.4 variant). `mur fleet limits <name> …` and `mur agent limits <name> …` are aliases: add to `FleetAction` and `AgentAction` a `Limits { name, deadline, stuck, cost_usd, unset, json }` variant with the same doc strings, dispatching to the same `write_*` / `cmd_limits` functions with the target fixed.

- [ ] **4.5 Watch it pass** — `cargo nextest run -p mur-core --lib -E 'test(/limits/)'`. Expected: all Task 3 + Task 4 tests pass. Manual: `cargo run -p mur-core --bin mur -- limits --global --deadline 4h` against a **copy** of your config (`MUR_HOME=$(mktemp -d)` with a pasted config.yaml), then `diff` — only the `limits:` block may differ.

- [ ] **4.6 fmt + clippy across `mur-common`, `mur-core`, `mur-daemon`, `mur-agent-runtime`; Hub check last**, then **commit**:

```
feat(cli): mur limits writes one scope — fleet, agent, or config.yaml as text

`mur limits <name> --deadline 2h --stuck off --unset cost_usd` edits the
named fleet's or agent's limits: block; `--global` edits config.yaml's.
The global write is textual — an existing block is replaced in place, a
missing one appended, and every block another binary owns survives
byte-for-byte, because the typed Config drops what it does not know.
A legacy fleet's first write materialises the block `mur limits` showed.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## After the last task

- One PR: `feat: limits: schema, three-scope resolution, and mur limits (spec step 3)`.
- Manual checks after `build.sh --install`: `mur limits deep-research`, `mur limits dr_worker_1` (expect `hitl.*` stale rows if present), `mur limits mur --json`, `mur limits --global --stuck 15m` then `git diff`-style comparison of `~/.mur/config.yaml` showing only the block changed.
- Docs via `update-docs`: README command tree gains `limits`; the docs site gets a `limits` page (register the slug in `SLUG_TO_FILE`, add to Features nav).
- Hub (step 3b) starts here: `limits_resolve` can wrap `cmd::limits::report` directly.
