# MUR Hub Fleet Manager Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Expose the fleet's three orthogonal axes — mode (plain/speculative/partition), run cadence (once/loop/auto), and worktree isolation (on/off) — in the MUR Hub GUI, which today only supports plain-squad fleets with a single one-shot Run button.

**Architecture:** Backend additions are small and mostly additive (`cmd_fleet_create` already accepts a `parallel` param; only loop-config mutation and a config-backed auto-run gate are new). The Hub Tauri layer stays a thin shim per the existing `mur-hub-gui/src-tauri/src/fleet.rs` convention — no business logic in the GUI process. Frontend work extends the existing `FleetCreateModal`/`FleetDetail`/`GeneralSettings` components in place, following their established state/CSS/i18n conventions exactly.

**Tech Stack:** Rust (mur-common, mur-core, mur-daemon), Tauri 2 (mur-hub-gui/src-tauri), React + TypeScript (mur-hub-gui/ui), vitest (frontend tests), cargo nextest (Rust tests — this workspace uses nextest, not plain `cargo test`; see Global Constraints).

## Global Constraints

- Spec: `docs/superpowers/specs/2026-07-01-mur-hub-fleet-manager-redesign-design.md`. Read it before starting if anything below is ambiguous.
- **Build env:** `export MUR_WEB_DIST=$HOME/Projects/mur-web/dist` and `export ORT_STRATEGY=download` before any `cargo build/check` that touches `mur-core` (it embeds the web dashboard and links onnxruntime). Ensure `~/.rustup/toolchains/stable-*/bin` is on `PATH` if `cargo`/`rustup` seem to be missing tools mid-session.
- **Test runner:** use `cargo nextest run -p <crate> <test_name>`, not plain `cargo test --workspace` (the latter has known spurious failures in this workspace unrelated to this feature).
- **Deadline/interval values are NOT calendar dates.** `parse_duration` (`mur-core/src/cmd/fleet/loop_run.rs:55`) only accepts digits + optional single-char suffix `s`/`m`/`h`/`d` (bare integer = seconds). Unparseable input silently means "no deadline" / "never fires" on the backend (fail-open) — so any UI surface for these two fields MUST validate client-side with the matching format and block submission on mismatch, never silently send a bad value through.
- **Isolation (worktree) only applies to the one-shot Run path.** `cmd_fleet_run_loop`/`run_guarded` (`mur-core/src/cmd/fleet/loop_run.rs`) have no Tier-1 worktree logic at all — do not add a worktree control anywhere near the "Run as loop" UI.
- **`cmd_fleet_set_loop` must use merge semantics** (load existing `loop_cfg`, overwrite only the fields actually provided as `Some(..)`, save). Never construct a fresh `FleetLoop` from scratch and overwrite the whole block — that would silently clobber fields a partial CLI/Hub update didn't intend to touch.
- **Tauri IPC argument casing:** Rust command params are snake_case; the JS `invoke()` call site uses camelCase for the same arguments (confirmed convention — e.g. Rust `out_path` ↔ JS `outPath` in `export_muragent_file`). Get this right in every new command or it fails at runtime with a missing-argument error, not a compile error.
- **i18n:** every new user-facing string needs a key in BOTH `mur-hub-gui/ui/src/i18n/en.ts` AND `mur-hub-gui/ui/src/i18n/zh-TW.ts` — `Table = Record<TranslationKey, string>` (`i18n/types.ts`) makes a missing zh-TW key a TypeScript compile error, not a silent gap.
- **Brand name:** nowhere in this feature does "MUR"/"Mur" appear in new user-facing copy, so rule 7 (uppercase MUR) doesn't come up — just flagging it's been checked.
- `mur-common/src/config.rs` is already 2022 lines (pre-existing, far past the 800-line guideline). Do NOT attempt to split it as part of this plan — that's an unrelated, separately-scoped cleanup. Add the new `FleetConfig` struct in place, following the file's existing per-section pattern (struct + a small `#[cfg(test)] mod <section>_tests` immediately after it).

---

## Task 1: Daemon auto-run config gate

**Files:**
- Modify: `mur-common/src/config.rs:86-101` (insert new struct after `ParallelJobsConfig`)
- Modify: `mur-daemon/src/fleet_tick.rs:144-157` (gate function + call site)
- Test: inline `#[cfg(test)]` modules in both files above

**Interfaces:**
- Produces: `mur_common::config::FleetConfig { pub autorun: bool }`, field `Config.fleet: FleetConfig`
- Produces: `fleet_tick::auto_run_enabled(mur_home: &Path) -> bool` (signature change — was zero-arg)
- Consumes: nothing from other tasks (fully independent)

- [ ] **Step 1: Write the failing config.rs test**

In `mur-common/src/config.rs`, immediately after the `ParallelJobsConfig` struct (ends at line 101, right before the `CcProxyConfig` doc comment), insert:

```rust
/// Daemon-wide gate for unattended fleet auto-run (`mur-daemon`'s `fleet_tick`).
/// Stored under `fleet:` in `~/.mur/config.yaml`. Either this flag OR the
/// `MUR_FLEET_AUTORUN` env var satisfies the gate — both are equally explicit,
/// off-by-default opt-ins; the env var remains for ops/CI use, this flag is
/// what the Hub's Settings toggle controls. Per-fleet `budget_usd > 0` and the
/// `.stopped` kill-switch are unaffected — see `mur-daemon/src/fleet_tick.rs`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FleetConfig {
    /// Allow fleets with a trigger + budget configured to auto-run unattended.
    #[serde(default)]
    pub autorun: bool,
}

#[cfg(test)]
mod fleet_config_tests {
    use super::*;

    #[test]
    fn fleet_config_defaults_off_and_roundtrips() {
        assert!(!FleetConfig::default().autorun);

        let cfg: Config = serde_yaml_ng::from_str("fleet:\n  autorun: true\n").unwrap();
        assert!(cfg.fleet.autorun);

        // `fleet:` key entirely absent → defaults to off
        let cfg2: Config = serde_yaml_ng::from_str("{}").unwrap();
        assert!(!cfg2.fleet.autorun);
    }
}
```

This won't compile yet — `Config` has no `fleet` field.

- [ ] **Step 2: Run it to confirm the compile failure**

Run: `cargo nextest run -p mur-common fleet_config_defaults_off_and_roundtrips`
Expected: FAIL — `error[E0560]: struct \`Config\` has no field named \`fleet\`` (or similar; the test module itself won't build).

- [ ] **Step 3: Add the `fleet` field to `Config`**

In `mur-common/src/config.rs`, the `Config` struct currently ends with (lines 85-88):

```rust
    // --- parallel_jobs MCP tool ---
    #[serde(default)]
    pub parallel_jobs: ParallelJobsConfig,
}
```

Change to:

```rust
    // --- parallel_jobs MCP tool ---
    #[serde(default)]
    pub parallel_jobs: ParallelJobsConfig,

    // --- Hub Fleet Manager redesign ---
    #[serde(default)]
    pub fleet: FleetConfig,
}
```

- [ ] **Step 4: Run the config.rs test again**

Run: `cargo nextest run -p mur-common fleet_config_defaults_off_and_roundtrips`
Expected: PASS

- [ ] **Step 5: Write the failing fleet_tick.rs test**

In `mur-daemon/src/fleet_tick.rs`, inside the existing `#[cfg(test)] mod tests` block (after the `autorun_flag_off_by_default` test, which ends around line 234), insert:

```rust
    #[test]
    fn auto_run_enabled_checks_env_var_or_config_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        unsafe { std::env::remove_var("MUR_FLEET_AUTORUN") };

        // neither env nor config → false
        assert!(!auto_run_enabled(home));

        // config flag alone → true
        std::fs::write(home.join("config.yaml"), "fleet:\n  autorun: true\n").unwrap();
        assert!(auto_run_enabled(home));

        // config flag false + env var set → true (env var still satisfies the gate)
        std::fs::write(home.join("config.yaml"), "fleet:\n  autorun: false\n").unwrap();
        unsafe { std::env::set_var("MUR_FLEET_AUTORUN", "1") };
        assert!(auto_run_enabled(home));

        unsafe { std::env::remove_var("MUR_FLEET_AUTORUN") };
    }
```

This won't compile yet — `auto_run_enabled()` takes no arguments today.

- [ ] **Step 6: Run it to confirm the compile failure**

Run: `cargo nextest run -p mur-daemon auto_run_enabled_checks_env_var_or_config_flag`
Expected: FAIL — `error[E0061]: this function takes 0 arguments but 1 argument was supplied`

- [ ] **Step 7: Change `auto_run_enabled` to take `mur_home` and check both gates**

In `mur-daemon/src/fleet_tick.rs`, this exact block (lines 148-150):

```rust
fn auto_run_enabled() -> bool {
    autorun_flag(std::env::var("MUR_FLEET_AUTORUN").ok().as_deref())
}
```

Change to:

```rust
fn auto_run_enabled(mur_home: &Path) -> bool {
    autorun_flag(std::env::var("MUR_FLEET_AUTORUN").ok().as_deref())
        || mur_common::config::Config::load_or_default(&mur_home.join("config.yaml"))
            .fleet
            .autorun
}
```

Then update the one call site, `tick()` (line 152-157):

```rust
pub fn tick(mur_home: &Path) {
    // Safety gate: unattended auto-run is OFF by default (best-practice audit;
    // OWASP Agentic ASI06 excessive agency). Opt in with `MUR_FLEET_AUTORUN=1`.
    if !auto_run_enabled() {
        return;
    }
```

Change the condition to:

```rust
    if !auto_run_enabled(mur_home) {
```

- [ ] **Step 8: Run both tests, confirm they pass, then the whole file's suite**

Run: `cargo nextest run -p mur-daemon auto_run_enabled_checks_env_var_or_config_flag`
Expected: PASS

Run: `cargo nextest run -p mur-daemon fleet_tick`
Expected: all `fleet_tick` tests PASS (confirms the signature change didn't break `autorun_flag_off_by_default` or anything else in the file — it doesn't call `auto_run_enabled` directly).

Run: `cargo nextest run -p mur-common fleet_config`
Expected: PASS

- [ ] **Step 9: Commit**

```bash
git add mur-common/src/config.rs mur-daemon/src/fleet_tick.rs
git commit -m "feat(fleet): config.yaml gate for daemon auto-run, alongside MUR_FLEET_AUTORUN"
```

---

## Task 2: `cmd_fleet_set_loop` + CLI `set-loop`

**Files:**
- Create: `mur-core/src/cmd/fleet/settings.rs`
- Modify: `mur-core/src/cmd/fleet/mod.rs:16-17` (register the new module)
- Modify: `mur-core/src/cli/actions.rs:404-444` (new `SetLoop` variant on `FleetAction`)
- Modify: `mur-core/src/dispatch.rs:269-306` (dispatch the new variant)

**Interfaces:**
- Produces: `mur_core::cmd::fleet::settings::cmd_fleet_set_loop(mur_home: &Path, name: &str, trigger: Option<String>, max_iterations: Option<u32>, deadline: Option<String>, budget_usd: Option<f64>, done_when: Option<String>) -> Result<()>`
- Consumes: `mur_common::fleet::FleetLoop { trigger, max_iterations, budget_usd, deadline, done_when }`, `store::{load_fleet, save_fleet}` (existing)
- Independent of Task 1 and Task 3.

- [ ] **Step 1: Write the failing test for `cmd_fleet_set_loop`**

Create `mur-core/src/cmd/fleet/settings.rs`:

```rust
//! `mur fleet set-loop` — mutate a fleet's `loop:` block (trigger, budget,
//! iteration cap, deadline, done-when marker) without touching anything else.

use std::path::Path;

use anyhow::Result;
use mur_common::fleet::FleetLoop;

use super::store;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_loop_merges_onto_existing_config_without_clobbering_other_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        super::super::create::cmd_fleet_create(
            home,
            "dev",
            vec!["pm".into()],
            None,
            Some("g".into()),
            None,
        )
        .unwrap();

        // First call sets trigger + budget; other fields take their defaults.
        cmd_fleet_set_loop(
            home,
            "dev",
            Some("interval:30m".into()),
            None,
            None,
            Some(5.0),
            None,
        )
        .unwrap();
        let f = store::load_fleet(home, "dev").unwrap();
        let l = f.loop_cfg.as_ref().unwrap();
        assert_eq!(l.trigger, "interval:30m");
        assert_eq!(l.budget_usd, 5.0);
        assert_eq!(l.max_iterations, 0);
        assert_eq!(l.deadline, "");
        assert_eq!(l.done_when, "");

        // Second call only touches max_iterations — trigger/budget set above MUST survive.
        cmd_fleet_set_loop(home, "dev", None, Some(10), None, None, None).unwrap();
        let f2 = store::load_fleet(home, "dev").unwrap();
        let l2 = f2.loop_cfg.as_ref().unwrap();
        assert_eq!(l2.max_iterations, 10);
        assert_eq!(l2.trigger, "interval:30m", "must not be clobbered");
        assert_eq!(l2.budget_usd, 5.0, "must not be clobbered");
    }

    #[test]
    fn set_loop_on_fleet_with_no_prior_loop_cfg_uses_field_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        super::super::create::cmd_fleet_create(
            home,
            "dev",
            vec!["pm".into()],
            None,
            Some("g".into()),
            None,
        )
        .unwrap();

        cmd_fleet_set_loop(home, "dev", None, None, None, Some(2.0), None).unwrap();
        let f = store::load_fleet(home, "dev").unwrap();
        let l = f.loop_cfg.as_ref().unwrap();
        assert_eq!(l.trigger, "manual", "untouched field uses FleetLoop's own default");
        assert_eq!(l.budget_usd, 2.0);
    }
}
```

This won't compile — `cmd_fleet_set_loop` doesn't exist yet.

- [ ] **Step 2: Register the module so the test can be found**

In `mur-core/src/cmd/fleet/mod.rs`, the module list currently reads (alphabetical):

```rust
pub mod roster;
pub mod run;
pub mod show;
```

Insert `settings` between `run` and `show`:

```rust
pub mod roster;
pub mod run;
pub mod settings;
pub mod show;
```

- [ ] **Step 3: Run the tests to confirm the compile failure**

Run: `cargo nextest run -p mur-core set_loop_merges_onto_existing_config`
Expected: FAIL — `cannot find function \`cmd_fleet_set_loop\` in this scope`

- [ ] **Step 4: Implement `cmd_fleet_set_loop`**

In `mur-core/src/cmd/fleet/settings.rs`, add the function above the `#[cfg(test)]` block:

```rust
/// Mutate a fleet's `loop:` block. Only the fields passed as `Some(..)` are
/// changed — everything else in the existing `loop_cfg` (or `FleetLoop`'s own
/// per-field defaults, if the fleet has no `loop_cfg` yet) is preserved. This
/// merge semantics matters: a partial update (e.g. "just bump the budget")
/// must never silently reset the trigger/deadline/done_when someone already set.
pub fn cmd_fleet_set_loop(
    mur_home: &Path,
    name: &str,
    trigger: Option<String>,
    max_iterations: Option<u32>,
    deadline: Option<String>,
    budget_usd: Option<f64>,
    done_when: Option<String>,
) -> Result<()> {
    let mut fleet = store::load_fleet(mur_home, name)?;
    let mut lc = fleet.loop_cfg.clone().unwrap_or(FleetLoop {
        trigger: "manual".to_string(),
        max_iterations: 0,
        budget_usd: 0.0,
        deadline: String::new(),
        done_when: String::new(),
    });
    if let Some(t) = trigger {
        lc.trigger = t;
    }
    if let Some(m) = max_iterations {
        lc.max_iterations = m;
    }
    if let Some(d) = deadline {
        lc.deadline = d;
    }
    if let Some(b) = budget_usd {
        lc.budget_usd = b;
    }
    if let Some(dw) = done_when {
        lc.done_when = dw;
    }
    fleet.loop_cfg = Some(lc);
    store::save_fleet(mur_home, &fleet)?;
    Ok(())
}
```

- [ ] **Step 5: Run the tests, confirm they pass**

Run: `cargo nextest run -p mur-core set_loop`
Expected: both tests PASS

- [ ] **Step 6: Add the CLI `set-loop` subcommand**

In `mur-core/src/cli/actions.rs`, the `FleetAction::Run` variant ends at line 444 (the closing `},` after `budget_usd: Option<f64>,`). Insert a new variant immediately after it, before `/// Queue a job for a fleet... Send {`:

```rust
    /// Update a fleet's loop/auto-run config (trigger, budget, iteration cap,
    /// deadline, done-when marker). Only the flags you pass are changed —
    /// everything else already set is preserved.
    SetLoop {
        /// Fleet name
        name: String,
        /// manual | interval:<dur> | cron:<5-field POSIX expr>
        #[arg(long)]
        trigger: Option<String>,
        /// Iteration cap for the guarded loop
        #[arg(long)]
        max_iterations: Option<u32>,
        /// Wall-clock deadline, e.g. 30s/5m/2h/1d (relative, NOT a calendar date)
        #[arg(long)]
        deadline: Option<String>,
        /// Projected USD budget ceiling for the loop; required > 0 for daemon auto-run
        #[arg(long)]
        budget_usd: Option<f64>,
        /// Convergence marker: `marker:<TEXT>` (own-line sentinel), or leave unset for router judgment
        #[arg(long)]
        done_when: Option<String>,
    },
```

- [ ] **Step 7: Wire the dispatch arm**

In `mur-core/src/dispatch.rs`, the `FleetAction::Run { ... } => { ... }` arm ends at line 306 (closing `}`). Insert immediately after it, before `FleetAction::Send { name, job } => {`:

```rust
                FleetAction::SetLoop {
                    name,
                    trigger,
                    max_iterations,
                    deadline,
                    budget_usd,
                    done_when,
                } => cmd::fleet::settings::cmd_fleet_set_loop(
                    &mur_home,
                    &name,
                    trigger,
                    max_iterations,
                    deadline,
                    budget_usd,
                    done_when,
                )?,
```

- [ ] **Step 8: Build and smoke-test the CLI end to end**

Run: `cargo build -p mur-core --bin mur` (ensure `MUR_WEB_DIST` and `ORT_STRATEGY=download` are set per Global Constraints)
Expected: builds cleanly.

Run (against a scratch `MUR_HOME`, e.g. `export MUR_HOME=/tmp/mur-plan-smoke && rm -rf $MUR_HOME`):
```bash
./target/debug/mur agent create --name pm --no-start 2>/dev/null || true
./target/debug/mur fleet create smoke --members pm --goal "test"
./target/debug/mur fleet set-loop smoke --trigger interval:30m --budget-usd 5
cat $MUR_HOME/fleets/smoke/fleet.yaml
```
Expected output includes:
```yaml
loop:
  trigger: interval:30m
  budget_usd: 5.0
```
(exact key order/formatting may differ — what matters is `trigger`/`budget_usd` are present with these values and the rest of the file — `name`, `members`, `channel_id`, `goal` — is untouched).

- [ ] **Step 9: Commit**

```bash
git add mur-core/src/cmd/fleet/settings.rs mur-core/src/cmd/fleet/mod.rs mur-core/src/cli/actions.rs mur-core/src/dispatch.rs
git commit -m "feat(fleet): cmd_fleet_set_loop + mur fleet set-loop CLI command"
```

---

## Task 3: Explicit worktree-isolation override for one-shot run

**Files:**
- Modify: `mur-core/src/cmd/fleet/run.rs:159-161,239,268,402-409` (signature + gate + test)
- Modify: `mur-core/src/cli/actions.rs:427-444` (`--worktree` flag on `Run`)
- Modify: `mur-core/src/dispatch.rs:282-306` (reject `--worktree` + `--loop` combo; pass the flag through)

**Interfaces:**
- Produces: `parallel_exec_enabled(force: bool) -> bool` (was zero-arg)
- Produces: `cmd_fleet_run(mur_home: &Path, name: &str, job_arg: Option<String>, force_worktree: bool) -> Result<()>` (new 4th param)
- Independent of Task 1 and Task 2.

- [ ] **Step 1: Write the failing test for the updated `parallel_exec_enabled`**

In `mur-core/src/cmd/fleet/run.rs`, the existing test (lines 402-409) reads:

```rust
    #[test]
    fn exec_flag_gates_parallel_execution() {
        unsafe { std::env::remove_var(EXEC_FLAG_ENV) };
        assert!(!parallel_exec_enabled());
        unsafe { std::env::set_var(EXEC_FLAG_ENV, "1") };
        assert!(parallel_exec_enabled());
        unsafe { std::env::remove_var(EXEC_FLAG_ENV) };
    }
```

Replace it with:

```rust
    #[test]
    fn exec_flag_gates_parallel_execution() {
        unsafe { std::env::remove_var(EXEC_FLAG_ENV) };
        assert!(!parallel_exec_enabled(false));
        unsafe { std::env::set_var(EXEC_FLAG_ENV, "1") };
        assert!(parallel_exec_enabled(false));
        unsafe { std::env::remove_var(EXEC_FLAG_ENV) };
    }

    #[test]
    fn force_worktree_bypasses_env_var() {
        unsafe { std::env::remove_var(EXEC_FLAG_ENV) };
        assert!(
            parallel_exec_enabled(true),
            "an explicit force=true must enable isolation even with the env var unset"
        );
        assert!(!parallel_exec_enabled(false));
    }
```

This won't compile — `parallel_exec_enabled` takes no arguments today.

- [ ] **Step 2: Run it to confirm the compile failure**

Run: `cargo nextest run -p mur-core exec_flag_gates_parallel_execution`
Expected: FAIL — `error[E0061]: this function takes 0 arguments but 1 argument was supplied`

- [ ] **Step 3: Update `parallel_exec_enabled` and `cmd_fleet_run`**

In `mur-core/src/cmd/fleet/run.rs`, this exact block (lines 159-161):

```rust
fn parallel_exec_enabled() -> bool {
    std::env::var(EXEC_FLAG_ENV).as_deref() == Ok("1")
}
```

Change to:

```rust
fn parallel_exec_enabled(force: bool) -> bool {
    force || std::env::var(EXEC_FLAG_ENV).as_deref() == Ok("1")
}
```

Then the function signature (line 239):

```rust
pub async fn cmd_fleet_run(mur_home: &Path, name: &str, job_arg: Option<String>) -> Result<()> {
```

Change to:

```rust
pub async fn cmd_fleet_run(
    mur_home: &Path,
    name: &str,
    job_arg: Option<String>,
    force_worktree: bool,
) -> Result<()> {
```

Then the one call site (line 268):

```rust
    let exec_parallel = parallel_exec_enabled() && fleet.parallel.is_some();
```

Change to:

```rust
    let exec_parallel = parallel_exec_enabled(force_worktree) && fleet.parallel.is_some();
```

Note: `run.rs`'s own test module calls `cmd_fleet_run` only indirectly through `resolve_run_goal`/`build_fleet_procedure`, never `cmd_fleet_run` itself — so no other test in this file breaks from the signature change. The only other call site is `mur-core/src/dispatch.rs`, which does NOT compile right now as a result of Step 3 — that's fixed next, in Step 4, before anything is run again.

- [ ] **Step 4: Add `--worktree` to the CLI and wire dispatch**

In `mur-core/src/cli/actions.rs`, the `Run` variant (lines 427-444) currently ends:

```rust
        /// Projected USD budget for the loop (overrides fleet.yaml `loop.budget_usd`)
        #[arg(long)]
        budget_usd: Option<f64>,
    },
```

Add a new field before the closing `},`:

```rust
        /// Projected USD budget for the loop (overrides fleet.yaml `loop.budget_usd`)
        #[arg(long)]
        budget_usd: Option<f64>,
        /// Force Tier-1 per-track git worktree isolation for this run (one-shot only,
        /// not supported with --loop). Equivalent to MUR_PARALLEL_EXEC=1 for this invocation.
        #[arg(long)]
        worktree: bool,
    },
```

In `mur-core/src/dispatch.rs`, the `FleetAction::Run { ... } => { ... }` arm (lines 282-306) currently reads:

```rust
                FleetAction::Run {
                    name,
                    job,
                    loop_flag,
                    max_iterations,
                    deadline,
                    budget_usd,
                } => {
                    if loop_flag {
                        // job arg + --loop: enqueue the job first, then the loop drains it.
                        if let Some(text) = job {
                            cmd::fleet::jobs::enqueue_job(&mur_home, &name, &text, "cli")?;
                        }
                        cmd::fleet::loop_run::cmd_fleet_run_loop(
                            &mur_home,
                            &name,
                            max_iterations,
                            deadline,
                            budget_usd,
                        )
                        .await?
                    } else {
                        cmd::fleet::run::cmd_fleet_run(&mur_home, &name, job).await?
                    }
                }
```

Replace with:

```rust
                FleetAction::Run {
                    name,
                    job,
                    loop_flag,
                    max_iterations,
                    deadline,
                    budget_usd,
                    worktree,
                } => {
                    if loop_flag {
                        if worktree {
                            anyhow::bail!(
                                "--worktree is not yet supported with --loop (the guarded-loop path has no worktree isolation)"
                            );
                        }
                        // job arg + --loop: enqueue the job first, then the loop drains it.
                        if let Some(text) = job {
                            cmd::fleet::jobs::enqueue_job(&mur_home, &name, &text, "cli")?;
                        }
                        cmd::fleet::loop_run::cmd_fleet_run_loop(
                            &mur_home,
                            &name,
                            max_iterations,
                            deadline,
                            budget_usd,
                        )
                        .await?
                    } else {
                        cmd::fleet::run::cmd_fleet_run(&mur_home, &name, job, worktree).await?
                    }
                }
```

- [ ] **Step 5: Run the full run.rs test suite**

Run: `cargo nextest run -p mur-core --lib cmd::fleet::run::tests`
Expected: all PASS (now that dispatch.rs compiles again).

Run: `cargo build -p mur-core --bin mur`
Expected: builds cleanly (confirms dispatch.rs + actions.rs compile together).

- [ ] **Step 6: Smoke-test the CLI rejection path**

Using the scratch `MUR_HOME` from Task 2 Step 8 (or a fresh one):
```bash
./target/debug/mur fleet run smoke --loop --worktree
```
Expected: prints an error containing "not yet supported with --loop" and exits non-zero (does NOT attempt to run anything).

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cmd/fleet/run.rs mur-core/src/cli/actions.rs mur-core/src/dispatch.rs
git commit -m "feat(fleet): explicit --worktree override for one-shot fleet run, decoupled from the env var"
```

---

## Task 4: Hub Tauri command layer

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/fleet.rs` (extend `fleet_create`/`fleet_run`; add `fleet_set_loop`, `fleet_run_loop`, `get_fleet_autorun`, `set_fleet_autorun`; extend `FleetDetail`)
- Modify: `mur-hub-gui/src-tauri/src/lib.rs:629-642` (register new commands)

**Interfaces:**
- Consumes: Task 1 (`mur_core::store::config::{load_config, save_config}`, `Config.fleet.autorun`), Task 2 (`settings::cmd_fleet_set_loop`), Task 3 (`run::cmd_fleet_run` 4-arg signature)
- Produces (new Tauri commands, JS-side camelCase args noted): `fleet_set_loop(name, trigger, maxIterations, deadline, budgetUsd, doneWhen)`, `fleet_run_loop(name, maxIterations, deadline, budgetUsd, app)`, `get_fleet_autorun()`, `set_fleet_autorun(enabled)`; extended `fleet_create(name, members, router, goal, parallel)`, `fleet_run(name, worktree, app)`; `FleetDetail` gains `loop_cfg: Option<FleetLoopView>` and `parallel_summary: Option<ParallelSummaryView>`.
- These are the exact JSON field names Tasks 5-8's frontend code must match: `FleetLoopView { trigger: String, max_iterations: u32, budget_usd: f64, deadline: String, done_when: String, last_run: Option<String> }`, `ParallelSummaryView { mode: String, track_count: usize, target_file: Option<String> }`.

- [ ] **Step 1: Write the failing tests for the new pure-logic pieces**

`fleet.rs`'s existing test module (lines 249-295) only tests pure helpers (`job_to_row`, `display`) — Tauri-annotated commands themselves aren't unit-tested in this file (no mock `AppHandle` harness exists; commands that need one are covered by the Computer-Use manual verification pass, not here). Add tests for the two new pure pieces this task introduces: the `parallel_summary` derivation and the `last_run` file read. In `fleet.rs`, inside the existing `#[cfg(test)] mod tests` block (after `display_falls_back_to_name`), add:

```rust
    #[test]
    fn parallel_summary_view_speculative_and_partition() {
        use mur_common::parallel::{JudgeConfig, ParallelConfig, ParallelMode, PartitionConfig, TrackConfig};

        let spec = ParallelConfig {
            mode: ParallelMode::Speculative,
            tracks: vec![
                TrackConfig { name: "a".into(), approach: "x".into(), model: None },
                TrackConfig { name: "b".into(), approach: "y".into(), model: None },
            ],
            judge: JudgeConfig { model: "claude-opus-4-8".into(), rubric: Default::default() },
            pre_filter: vec![],
            partition: None,
        };
        let view = parallel_summary_view(&spec);
        assert_eq!(view.mode, "speculative");
        assert_eq!(view.track_count, 2);
        assert_eq!(view.target_file, None);

        let part = ParallelConfig {
            mode: ParallelMode::Partition,
            tracks: vec![],
            judge: JudgeConfig { model: "claude-opus-4-8".into(), rubric: Default::default() },
            pre_filter: vec![],
            partition: Some(PartitionConfig { target_file: "src/widget.rs".into() }),
        };
        let view2 = parallel_summary_view(&part);
        assert_eq!(view2.mode, "partition");
        assert_eq!(view2.track_count, 0);
        assert_eq!(view2.target_file.as_deref(), Some("src/widget.rs"));
    }

    #[test]
    fn last_run_reads_sentinel_and_handles_absence() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let fleet_dir = store::fleet_dir(home, "dev");
        std::fs::create_dir_all(&fleet_dir).unwrap();

        assert_eq!(read_last_run_rfc3339(home, "dev"), None);

        std::fs::write(fleet_dir.join(".last_run"), "1751328000").unwrap();
        let got = read_last_run_rfc3339(home, "dev").unwrap();
        // RFC3339-parseable and round-trips to the same unix timestamp (avoids
        // hardcoding a guessed calendar year, which would be a flaky assertion).
        let parsed = chrono::DateTime::parse_from_rfc3339(&got).unwrap();
        assert_eq!(parsed.timestamp(), 1751328000);
    }
```

These won't compile — `parallel_summary_view`, `read_last_run_rfc3339`, and `ParallelSummaryView` don't exist yet.

- [ ] **Step 2: Run the tests to confirm the compile failure**

`mur-hub-gui` is workspace-excluded (per `CLAUDE.md`), so `-p mur-hub-gui` will not resolve — invoke it via its own manifest path instead:

Run: `cargo nextest run --manifest-path mur-hub-gui/src-tauri/Cargo.toml parallel_summary_view`
Expected: FAIL to compile — functions/types not found.

- [ ] **Step 3: Implement the new types and helpers, extend `fleet_create`/`fleet_run`, extend `FleetDetail`**

In `mur-hub-gui/src-tauri/src/fleet.rs`, change the imports (line 6):

```rust
use mur_core::cmd::fleet::{control, create, delete, export, import, jobs, roster, run, store};
```

to:

```rust
use mur_core::cmd::fleet::{control, create, delete, export, import, jobs, loop_run, roster, run, settings, store};
use mur_common::parallel::ParallelConfig;
```

Replace the `FleetDetail` struct (lines 23-32):

```rust
#[derive(Serialize, Clone)]
pub struct FleetDetail {
    pub name: String,
    pub display_name: String,
    pub goal: String,
    pub router: String,
    pub members: Vec<String>,
    pub channel_id: String,
    pub stopped: bool,
}
```

with:

```rust
#[derive(Serialize, Clone)]
pub struct FleetLoopView {
    pub trigger: String,
    pub max_iterations: u32,
    pub budget_usd: f64,
    pub deadline: String,
    pub done_when: String,
    pub last_run: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct ParallelSummaryView {
    pub mode: String,
    pub track_count: usize,
    pub target_file: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct FleetDetail {
    pub name: String,
    pub display_name: String,
    pub goal: String,
    pub router: String,
    pub members: Vec<String>,
    pub channel_id: String,
    pub stopped: bool,
    pub loop_cfg: Option<FleetLoopView>,
    pub parallel_summary: Option<ParallelSummaryView>,
}

fn parallel_summary_view(cfg: &ParallelConfig) -> ParallelSummaryView {
    ParallelSummaryView {
        mode: match cfg.mode {
            mur_common::parallel::ParallelMode::Speculative => "speculative".to_string(),
            mur_common::parallel::ParallelMode::Partition => "partition".to_string(),
        },
        track_count: cfg.tracks.len(),
        target_file: cfg.partition.as_ref().map(|p| p.target_file.clone()),
    }
}

/// Read a fleet's `.last_run` auto-run sentinel (unix seconds, written by
/// `mur-daemon`'s `fleet_tick`) and format it as RFC3339, if present.
fn read_last_run_rfc3339(mur_home: &std::path::Path, name: &str) -> Option<String> {
    let secs: i64 = std::fs::read_to_string(store::fleet_dir(mur_home, name).join(".last_run"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    chrono::DateTime::from_timestamp(secs, 0).map(|dt| dt.to_rfc3339())
}
```

Now update `fleet_detail` (lines 100-114) to populate the two new fields. Replace:

```rust
#[tauri::command]
pub fn fleet_detail(name: String) -> Result<FleetDetail, String> {
    let home = mur_home_path();
    let fleet = store::load_fleet(&home, &name).map_err(|e| e.to_string())?;
    let stopped = control::is_stopped(&home, &name);
    Ok(FleetDetail {
        name: fleet.name.clone(),
        display_name: display(&fleet.name, &fleet.display_name),
        goal: fleet.goal.clone(),
        router: fleet.router_or_concierge().to_string(),
        members: fleet.members.clone(),
        channel_id: fleet.channel_id.clone(),
        stopped,
    })
}
```

with:

```rust
#[tauri::command]
pub fn fleet_detail(name: String) -> Result<FleetDetail, String> {
    let home = mur_home_path();
    let fleet = store::load_fleet(&home, &name).map_err(|e| e.to_string())?;
    let stopped = control::is_stopped(&home, &name);
    let loop_cfg = fleet.loop_cfg.as_ref().map(|l| FleetLoopView {
        trigger: l.trigger.clone(),
        max_iterations: l.max_iterations,
        budget_usd: l.budget_usd,
        deadline: l.deadline.clone(),
        done_when: l.done_when.clone(),
        last_run: read_last_run_rfc3339(&home, &name),
    });
    let parallel_summary = fleet.parallel.as_ref().map(parallel_summary_view);
    Ok(FleetDetail {
        name: fleet.name.clone(),
        display_name: display(&fleet.name, &fleet.display_name),
        goal: fleet.goal.clone(),
        router: fleet.router_or_concierge().to_string(),
        members: fleet.members.clone(),
        channel_id: fleet.channel_id.clone(),
        stopped,
        loop_cfg,
        parallel_summary,
    })
}
```

Update `fleet_create` (lines 116-126). Replace:

```rust
#[tauri::command]
pub fn fleet_create(
    name: String,
    members: Vec<String>,
    router: Option<String>,
    goal: String,
) -> Result<(), String> {
    let home = mur_home_path();
    create::cmd_fleet_create(&home, &name, members, router, Some(goal), None)
        .map_err(|e| e.to_string())
}
```

with:

```rust
#[tauri::command]
pub fn fleet_create(
    name: String,
    members: Vec<String>,
    router: Option<String>,
    goal: String,
    parallel: Option<ParallelConfig>,
) -> Result<(), String> {
    let home = mur_home_path();
    create::cmd_fleet_create(&home, &name, members, router, Some(goal), parallel)
        .map_err(|e| e.to_string())
}
```

Update `fleet_run` (lines 147-164). Replace:

```rust
#[tauri::command]
pub async fn fleet_run(name: String, app: tauri::AppHandle) -> Result<(), String> {
    let home = mur_home_path();
    let fleet_name = name.clone();
    // cmd_fleet_run is async but does blocking I/O internally (UnixStream dial).
    // Use spawn_blocking with a dedicated runtime so tokio worker threads aren't tied up.
    tokio::task::spawn_blocking(move || {
        let ok = tokio::runtime::Runtime::new()
            .expect("fleet run runtime")
            .block_on(run::cmd_fleet_run(&home, &fleet_name, None))
            .is_ok();
        let _ = app.emit(
            "fleet:run_done",
            serde_json::json!({ "name": fleet_name, "ok": ok }),
        );
    });
    Ok(())
}
```

with:

```rust
#[tauri::command]
pub async fn fleet_run(name: String, worktree: bool, app: tauri::AppHandle) -> Result<(), String> {
    let home = mur_home_path();
    let fleet_name = name.clone();
    // cmd_fleet_run is async but does blocking I/O internally (UnixStream dial).
    // Use spawn_blocking with a dedicated runtime so tokio worker threads aren't tied up.
    tokio::task::spawn_blocking(move || {
        let ok = tokio::runtime::Runtime::new()
            .expect("fleet run runtime")
            .block_on(run::cmd_fleet_run(&home, &fleet_name, None, worktree))
            .is_ok();
        let _ = app.emit(
            "fleet:run_done",
            serde_json::json!({ "name": fleet_name, "ok": ok }),
        );
    });
    Ok(())
}
```

Add four new commands after `fleet_run` and before `fleet_send`:

```rust
#[tauri::command]
pub async fn fleet_run_loop(
    name: String,
    max_iterations: Option<u32>,
    deadline: Option<String>,
    budget_usd: Option<f64>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let home = mur_home_path();
    let fleet_name = name.clone();
    tokio::task::spawn_blocking(move || {
        let ok = tokio::runtime::Runtime::new()
            .expect("fleet run loop runtime")
            .block_on(loop_run::cmd_fleet_run_loop(
                &home,
                &fleet_name,
                max_iterations,
                deadline,
                budget_usd,
            ))
            .is_ok();
        let _ = app.emit(
            "fleet:run_done",
            serde_json::json!({ "name": fleet_name, "ok": ok }),
        );
    });
    Ok(())
}

#[tauri::command]
pub fn fleet_set_loop(
    name: String,
    trigger: Option<String>,
    max_iterations: Option<u32>,
    deadline: Option<String>,
    budget_usd: Option<f64>,
    done_when: Option<String>,
) -> Result<(), String> {
    let home = mur_home_path();
    settings::cmd_fleet_set_loop(
        &home,
        &name,
        trigger,
        max_iterations,
        deadline,
        budget_usd,
        done_when,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_fleet_autorun() -> Result<bool, String> {
    let cfg = mur_core::store::config::load_config().map_err(|e| e.to_string())?;
    Ok(cfg.fleet.autorun)
}

#[tauri::command]
pub fn set_fleet_autorun(enabled: bool) -> Result<(), String> {
    let mut cfg = mur_core::store::config::load_config().map_err(|e| e.to_string())?;
    cfg.fleet.autorun = enabled;
    mur_core::store::config::save_config(&cfg).map_err(|e| e.to_string())
}
```

- [ ] **Step 4: Run the new tests, confirm they pass**

Run: `cargo nextest run --manifest-path mur-hub-gui/src-tauri/Cargo.toml parallel_summary_view`
Run: `cargo nextest run --manifest-path mur-hub-gui/src-tauri/Cargo.toml last_run_reads_sentinel`
Expected: both PASS.

- [ ] **Step 5: Run the full file's existing tests to confirm nothing else broke**

Run: `cargo nextest run --manifest-path mur-hub-gui/src-tauri/Cargo.toml --lib fleet::tests`
Expected: all PASS (`job_to_row_maps_status_to_string`, `display_falls_back_to_name`, plus the two new tests).

- [ ] **Step 6: Register the new commands in `lib.rs`**

In `mur-hub-gui/src-tauri/src/lib.rs`, the registration block (lines 629-642) currently reads:

```rust
            fleet::fleet_list,
            fleet::fleet_detail,
            fleet::fleet_create,
            fleet::fleet_delete,
            fleet::fleet_stop,
            fleet::fleet_start,
            fleet::fleet_run,
            fleet::fleet_send,
            fleet::fleet_jobs,
            fleet::fleet_add_member,
            fleet::fleet_remove_member,
            fleet::fleet_export,
            fleet::fleet_export_to,
            fleet::fleet_import,
        ])
```

Change to:

```rust
            fleet::fleet_list,
            fleet::fleet_detail,
            fleet::fleet_create,
            fleet::fleet_delete,
            fleet::fleet_stop,
            fleet::fleet_start,
            fleet::fleet_run,
            fleet::fleet_run_loop,
            fleet::fleet_set_loop,
            fleet::get_fleet_autorun,
            fleet::set_fleet_autorun,
            fleet::fleet_send,
            fleet::fleet_jobs,
            fleet::fleet_add_member,
            fleet::fleet_remove_member,
            fleet::fleet_export,
            fleet::fleet_export_to,
            fleet::fleet_import,
        ])
```

- [ ] **Step 7: Build the Tauri backend**

Run: `cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml`
Expected: builds cleanly. Cargo doesn't care whether Tasks 5-8's frontend TypeScript exists yet — it only needs `ui/dist/` to exist as a directory (for `tauri::generate_context!()`'s asset embedding). If that directory is missing and the build complains, stub `ui/dist/index.html` locally (don't commit it) — see `gotcha_hub_clippy_needs_ui_dist` if that memory is available.

- [ ] **Step 8: Commit**

```bash
git add mur-hub-gui/src-tauri/src/fleet.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(hub): Tauri commands for fleet loop settings, loop runs, and auto-run toggle"
```

---

## Task 5: FleetCreateModal Mode picker

**Files:**
- Modify: `mur-hub-gui/ui/src/components/fleet/FleetCreateModal.tsx` (entire file restructured)
- Modify: `mur-hub-gui/ui/src/styles/components/fleet.css` (append new rules)
- Modify: `mur-hub-gui/ui/src/i18n/en.ts:586-623` and `mur-hub-gui/ui/src/i18n/zh-TW.ts:588-625` (append new keys)

**Interfaces:**
- Consumes: Task 4's `fleet_create(name, members, router, goal, parallel)` Tauri command; existing `list_models` command + `ModelOption`/`groupByProvider` from `mur-hub-gui/ui/src/components/modelPicker.ts`.
- Produces: nothing other tasks depend on (FleetCreateModal is a leaf component; `FleetView.tsx` renders it unchanged).

- [ ] **Step 1: Write the failing test for the client-side validation + payload-building logic**

This component has no existing test file (`FleetCreateModal.test.tsx` doesn't exist) — the project's convention for this kind of pure-logic-inside-a-component code (see `modelPicker.ts`/`modelPicker.test.ts`) is to extract pure functions and test those directly rather than mount the component. Create `mur-hub-gui/ui/src/components/fleet/fleetCreateForm.test.ts` — note `fleetCreateForm.ts` (the implementation it imports) does NOT exist yet, that's Step 3:

```ts
import { describe, it, expect } from "vitest";
import { canSubmitMode, buildParallelPayload, DURATION_RE } from "./fleetCreateForm";

describe("canSubmitMode", () => {
  it("plain mode always submittable", () => {
    expect(canSubmitMode("plain", [], "", "")).toBe(true);
  });
  it("speculative needs >=2 tracks and a judge model", () => {
    expect(canSubmitMode("speculative", [], "claude-opus-4-8", "")).toBe(false);
    expect(canSubmitMode("speculative", [{ name: "a", approach: "", model: "" }], "claude-opus-4-8", "")).toBe(false);
    const twoTracks = [
      { name: "a", approach: "", model: "" },
      { name: "b", approach: "", model: "" },
    ];
    expect(canSubmitMode("speculative", twoTracks, "", "")).toBe(false); // missing judge model
    expect(canSubmitMode("speculative", twoTracks, "claude-opus-4-8", "")).toBe(true);
  });
  it("partition needs a target file and a judge model", () => {
    expect(canSubmitMode("partition", [], "claude-opus-4-8", "")).toBe(false);
    expect(canSubmitMode("partition", [], "", "src/widget.rs")).toBe(false);
    expect(canSubmitMode("partition", [], "claude-opus-4-8", "src/widget.rs")).toBe(true);
  });
});

describe("buildParallelPayload", () => {
  it("plain mode returns null", () => {
    expect(buildParallelPayload("plain", [], "", "", false, false)).toBeNull();
  });
  it("speculative builds tracks + judge + pre_filter", () => {
    const tracks = [
      { name: "track-a", approach: "functional style", model: "" },
      { name: "track-b", approach: "performance first", model: "claude-opus-4-8" },
    ];
    const payload = buildParallelPayload("speculative", tracks, "claude-opus-4-8", "", true, false);
    expect(payload).toEqual({
      mode: "speculative",
      tracks: [
        { name: "track-a", approach: "functional style", model: null },
        { name: "track-b", approach: "performance first", model: "claude-opus-4-8" },
      ],
      judge: { model: "claude-opus-4-8" },
      pre_filter: ["cargo_check"],
    });
  });
  it("partition builds target_file, empty tracks", () => {
    const payload = buildParallelPayload("partition", [], "claude-opus-4-8", "src/widget.rs", false, false);
    expect(payload).toEqual({
      mode: "partition",
      tracks: [],
      judge: { model: "claude-opus-4-8" },
      pre_filter: [],
      partition: { target_file: "src/widget.rs" },
    });
  });
});

describe("DURATION_RE", () => {
  it("accepts mur-core's parse_duration formats", () => {
    for (const v of ["30s", "5m", "2h", "1d", "8"]) {
      expect(DURATION_RE.test(v)).toBe(true);
    }
  });
  it("rejects calendar dates and unsupported units", () => {
    for (const v of ["2026-12-31", "1w", "", "abc"]) {
      expect(DURATION_RE.test(v)).toBe(false);
    }
  });
});
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cd mur-hub-gui/ui && npx vitest run src/components/fleet/fleetCreateForm.test.ts`
Expected: FAIL — `Cannot find module './fleetCreateForm'` (the file doesn't exist yet).

- [ ] **Step 3: Implement `fleetCreateForm.ts`**

Create `mur-hub-gui/ui/src/components/fleet/fleetCreateForm.ts`:

```ts
/**
 * Pure helpers for the fleet creation form's Mode section.
 * No DOM, no React — unit-testable, mirrored against modelPicker.ts's pattern.
 */

export type FleetMode = "plain" | "speculative" | "partition";

export interface TrackInput {
  name: string;
  approach: string;
  model: string;
}

export interface ParallelTrackPayload {
  name: string;
  approach: string;
  model: string | null;
}

export interface ParallelConfigPayload {
  mode: "speculative" | "partition";
  tracks: ParallelTrackPayload[];
  judge: { model: string };
  pre_filter?: string[];
  partition?: { target_file: string };
}

/** Matches mur-core's parse_duration: digits + optional single-char s/m/h/d suffix. */
export const DURATION_RE = /^\d+[smhd]?$/;

export function canSubmitMode(
  mode: FleetMode,
  tracks: TrackInput[],
  judgeModel: string,
  targetFile: string
): boolean {
  if (mode === "speculative") return tracks.length >= 2 && judgeModel.trim() !== "";
  if (mode === "partition") return targetFile.trim() !== "" && judgeModel.trim() !== "";
  return true;
}

export function buildParallelPayload(
  mode: FleetMode,
  tracks: TrackInput[],
  judgeModel: string,
  targetFile: string,
  preFilterCargoCheck: boolean,
  preFilterClippy: boolean
): ParallelConfigPayload | null {
  if (mode === "plain") return null;
  if (mode === "speculative") {
    return {
      mode: "speculative",
      tracks: tracks.map((t) => ({
        name: t.name,
        approach: t.approach,
        model: t.model.trim() || null,
      })),
      judge: { model: judgeModel.trim() },
      pre_filter: [
        ...(preFilterCargoCheck ? ["cargo_check"] : []),
        ...(preFilterClippy ? ["cargo_clippy_deny"] : []),
      ],
    };
  }
  return {
    mode: "partition",
    tracks: [],
    judge: { model: judgeModel.trim() },
    partition: { target_file: targetFile.trim() },
  };
}
```

- [ ] **Step 4: Run the test again, confirm it passes**

Run: `cd mur-hub-gui/ui && npx vitest run src/components/fleet/fleetCreateForm.test.ts`
Expected: PASS — all 4 describe blocks green.

- [ ] **Step 5: Rewrite `FleetCreateModal.tsx`**

Replace the entire file with:

```tsx
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../i18n";
import { groupByProvider, type ModelOption } from "../modelPicker";
import {
  canSubmitMode,
  buildParallelPayload,
  DURATION_RE,
  type FleetMode,
  type TrackInput,
} from "./fleetCreateForm";

interface Props {
  onCreated: (name: string) => void;
  onClose: () => void;
}

function ModelSelect({
  models,
  value,
  onChange,
  allowDefault,
}: {
  models: ModelOption[];
  value: string;
  onChange: (v: string) => void;
  allowDefault?: boolean;
}) {
  const { t } = useT();
  return (
    <select value={value} onChange={(e) => onChange(e.target.value)}>
      {allowDefault ? (
        <option value="">{t("fleet.create.modelDefault")}</option>
      ) : (
        <option value="" disabled>
          {t("fleet.create.chooseModel")}
        </option>
      )}
      {groupByProvider(models).map(([provider, opts]) => (
        <optgroup key={provider} label={provider}>
          {opts.map((m) => (
            <option key={m.ref_name} value={m.model}>
              {m.model}
            </option>
          ))}
        </optgroup>
      ))}
    </select>
  );
}

export function FleetCreateModal({ onCreated, onClose }: Props) {
  const { t } = useT();
  const [name, setName] = useState("");
  const [goal, setGoal] = useState("");
  const [members, setMembers] = useState("");
  const [router, setRouter] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const [mode, setMode] = useState<FleetMode>("plain");
  const [models, setModels] = useState<ModelOption[]>([]);
  const [judgeModel, setJudgeModel] = useState("");
  const [tracks, setTracks] = useState<TrackInput[]>([]);
  const [preFilterCargoCheck, setPreFilterCargoCheck] = useState(false);
  const [preFilterClippy, setPreFilterClippy] = useState(false);
  const [targetFile, setTargetFile] = useState("");

  useEffect(() => {
    invoke<ModelOption[]>("list_models").then(setModels).catch(() => {});
  }, []);

  function handleModeChange(next: FleetMode) {
    setMode(next);
    if (next === "speculative" && tracks.length === 0) {
      setTracks([
        { name: "track-a", approach: "", model: "" },
        { name: "track-b", approach: "", model: "" },
      ]);
    }
  }

  function updateTrack(i: number, patch: Partial<TrackInput>) {
    setTracks((prev) => prev.map((t, idx) => (idx === i ? { ...t, ...patch } : t)));
  }

  function addTrack() {
    setTracks((prev) => [...prev, { name: `track-${prev.length}`, approach: "", model: "" }]);
  }

  function removeTrack(i: number) {
    setTracks((prev) => prev.filter((_, idx) => idx !== i));
  }

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    setError(null);
    if (!canSubmitMode(mode, tracks, judgeModel, targetFile)) {
      setError(t("fleet.create.modeIncomplete"));
      return;
    }
    setBusy(true);
    const memberList = members
      .split(",")
      .map((m) => m.trim())
      .filter(Boolean);
    try {
      await invoke("fleet_create", {
        name: name.trim(),
        goal: goal.trim(),
        members: memberList,
        router: router.trim() || null,
        parallel: buildParallelPayload(
          mode,
          tracks,
          judgeModel,
          targetFile,
          preFilterCargoCheck,
          preFilterClippy
        ),
      });
      onCreated(name.trim());
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal-card" onClick={(e) => e.stopPropagation()}>
        <h2>{t("fleet.new")}</h2>
        <form onSubmit={handleSubmit}>
          <label className="field">
            <span>{t("fleet.create.name")}</span>
            <input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="dev-squad"
              required
              pattern="[a-z0-9_-]+"
              title="Lowercase letters, digits, - or _"
              autoFocus
            />
          </label>
          <label className="field">
            <span>{t("fleet.create.goal")}</span>
            <input
              value={goal}
              onChange={(e) => setGoal(e.target.value)}
              placeholder="Ship the v3 release"
              required
            />
          </label>
          <label className="field">
            <span>{t("fleet.create.members")}</span>
            <input
              value={members}
              onChange={(e) => setMembers(e.target.value)}
              placeholder="pm, qa, dev"
              required
            />
          </label>
          <label className="field">
            <span>{t("fleet.create.router")}</span>
            <input
              value={router}
              onChange={(e) => setRouter(e.target.value)}
              placeholder="mur"
            />
          </label>

          <div className="fleet-create__mode">
            <span className="fleet-section__label">{t("fleet.create.mode.label")}</span>
            {(["plain", "speculative", "partition"] as FleetMode[]).map((m) => (
              <label key={m} className="fleet-create__mode-option">
                <input
                  type="radio"
                  name="fleet-mode"
                  checked={mode === m}
                  onChange={() => handleModeChange(m)}
                />
                <span>
                  {t(`fleet.create.mode.${m}` as Parameters<typeof t>[0])}
                  <span className="fleet-create__mode-desc">
                    {t(`fleet.create.mode.${m}Desc` as Parameters<typeof t>[0])}
                  </span>
                </span>
              </label>
            ))}
          </div>

          {mode === "speculative" && (
            <div className="fleet-create__section">
              <label className="field">
                <span>{t("fleet.create.judgeModel")}</span>
                <ModelSelect models={models} value={judgeModel} onChange={setJudgeModel} />
              </label>
              <span className="fleet-section__label">{t("fleet.create.tracks")}</span>
              {tracks.map((track, i) => (
                <div key={i} className="fleet-create__track">
                  <input
                    value={track.approach}
                    onChange={(e) => updateTrack(i, { approach: e.target.value })}
                    placeholder={t("fleet.create.trackApproach")}
                  />
                  <ModelSelect
                    models={models}
                    value={track.model}
                    onChange={(v) => updateTrack(i, { model: v })}
                    allowDefault
                  />
                  <button type="button" onClick={() => removeTrack(i)}>
                    ✕
                  </button>
                </div>
              ))}
              <button type="button" className="toolbar-btn" onClick={addTrack}>
                {t("fleet.create.addTrack")}
              </button>
              <div className="fleet-create__prefilters">
                <span>{t("fleet.create.preFilter")}</span>
                <label>
                  <input
                    type="checkbox"
                    checked={preFilterCargoCheck}
                    onChange={(e) => setPreFilterCargoCheck(e.target.checked)}
                  />
                  {t("fleet.create.preFilterCargoCheck")}
                </label>
                <label>
                  <input
                    type="checkbox"
                    checked={preFilterClippy}
                    onChange={(e) => setPreFilterClippy(e.target.checked)}
                  />
                  {t("fleet.create.preFilterClippy")}
                </label>
              </div>
            </div>
          )}

          {mode === "partition" && (
            <div className="fleet-create__section">
              <label className="field">
                <span>{t("fleet.create.judgeModel")}</span>
                <ModelSelect models={models} value={judgeModel} onChange={setJudgeModel} />
                <span className="fleet-create__mode-desc">{t("fleet.create.judgeModelPartitionHint")}</span>
              </label>
              <label className="field">
                <span>{t("fleet.create.targetFile")}</span>
                <input
                  value={targetFile}
                  onChange={(e) => setTargetFile(e.target.value)}
                  placeholder="src/widget.rs"
                />
                <span className="fleet-create__mode-desc">{t("fleet.create.targetFileHint")}</span>
              </label>
            </div>
          )}

          {error && <p className="field-error">{error}</p>}
          <div className="modal-actions">
            <button type="button" onClick={onClose} disabled={busy}>
              {t("detail.close")}
            </button>
            <button
              type="submit"
              className="toolbar-btn toolbar-btn--primary"
              disabled={busy || !canSubmitMode(mode, tracks, judgeModel, targetFile)}
            >
              {busy ? "…" : t("fleet.create.submit")}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
```

Note: `DURATION_RE` is imported but not used in this file directly — it's exported from `fleetCreateForm.ts` for Task 6 to reuse (same validation, the Settings section). Remove the unused import here, OR leave it imported and used nowhere — TypeScript's `noUnusedLocals` (if enabled) would error. Check `mur-hub-gui/ui/tsconfig.json` for `noUnusedLocals`; if set, drop `DURATION_RE` from this file's import list (it isn't used here, only re-exported via the module — importing a name only to make it "available" isn't necessary in ES modules, Task 6 imports it directly from `./fleetCreateForm`, not through this file).

- [ ] **Step 6: Add the CSS**

Append to `mur-hub-gui/ui/src/styles/components/fleet.css`:

```css
/* ── Create modal: Mode picker ─────────────────────────────────────────── */

.fleet-create__mode {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin: 12px 0;
}

.fleet-create__mode-option {
  display: flex;
  align-items: flex-start;
  gap: 8px;
  padding: 8px 10px;
  border-radius: var(--radius-md, 8px);
  background: var(--bg-card, rgba(255,255,255,0.03));
  cursor: pointer;
  font-size: 12px;
}

.fleet-create__mode-option input { margin-top: 2px; }

.fleet-create__mode-desc {
  display: block;
  color: var(--text-tertiary);
  font-size: 11px;
}

.fleet-create__section {
  margin: 10px 0;
  padding: 10px 12px;
  border: 1px solid var(--border-line);
  border-radius: var(--radius-md, 8px);
}

.fleet-create__track {
  display: flex;
  gap: 6px;
  align-items: center;
  margin-bottom: 6px;
}

.fleet-create__track input { flex: 1; font-size: 12px; padding: 6px 8px; }
.fleet-create__track select { font-size: 12px; }

.fleet-create__prefilters {
  display: flex;
  gap: 12px;
  font-size: 12px;
  color: var(--text-secondary);
  align-items: center;
}
```

- [ ] **Step 7: Add i18n keys**

In `mur-hub-gui/ui/src/i18n/en.ts`, after line 623 (`"fleet.create.submit": "Create Fleet",`), insert:

```ts
  "fleet.create.mode.label": "Mode",
  "fleet.create.mode.plain": "Plain",
  "fleet.create.mode.plainDesc": "Broadcast the goal to every member",
  "fleet.create.mode.speculative": "Speculative",
  "fleet.create.mode.speculativeDesc": "N tracks race the same goal, judge picks the best",
  "fleet.create.mode.partition": "Partition",
  "fleet.create.mode.partitionDesc": "Split one file into disjoint regions, one track per region",
  "fleet.create.modeIncomplete": "Fill in the required fields for this mode before creating.",
  "fleet.create.judgeModel": "Judge model",
  "fleet.create.judgeModelPartitionHint": "Also used by `fleet compare`/`cherry` to score tracks",
  "fleet.create.tracks": "Tracks",
  "fleet.create.trackApproach": "Approach",
  "fleet.create.addTrack": "+ Add track",
  "fleet.create.preFilter": "Pre-filters",
  "fleet.create.preFilterCargoCheck": "cargo check",
  "fleet.create.preFilterClippy": "cargo clippy (deny warnings)",
  "fleet.create.targetFile": "Target file",
  "fleet.create.targetFileHint": "Repo-relative path; regions are auto-derived from this file",
  "fleet.create.chooseModel": "— choose a model —",
  "fleet.create.modelDefault": "(default)",
```

In `mur-hub-gui/ui/src/i18n/zh-TW.ts`, after line 625 (`"fleet.create.submit": "建立機群",`), insert:

```ts
  "fleet.create.mode.label": "類型",
  "fleet.create.mode.plain": "一般",
  "fleet.create.mode.plainDesc": "將目標廣播給每位成員",
  "fleet.create.mode.speculative": "競速",
  "fleet.create.mode.speculativeDesc": "多條軌道同時挑戰同一目標，由評審選出最佳結果",
  "fleet.create.mode.partition": "分割",
  "fleet.create.mode.partitionDesc": "將單一檔案切分為不重疊的區塊，每條軌道負責一塊",
  "fleet.create.modeIncomplete": "請先填寫此類型所需的欄位再建立機群。",
  "fleet.create.judgeModel": "評審模型",
  "fleet.create.judgeModelPartitionHint": "`fleet compare`／`cherry` 也會用它為各軌道評分",
  "fleet.create.tracks": "軌道",
  "fleet.create.trackApproach": "做法",
  "fleet.create.addTrack": "+ 新增軌道",
  "fleet.create.preFilter": "前置過濾",
  "fleet.create.preFilterCargoCheck": "cargo check",
  "fleet.create.preFilterClippy": "cargo clippy（拒絕警告）",
  "fleet.create.targetFile": "目標檔案",
  "fleet.create.targetFileHint": "相對於專案根目錄的路徑；區塊會依此檔案自動切分",
  "fleet.create.chooseModel": "— 請選擇模型 —",
  "fleet.create.modelDefault": "（預設）",
```

- [ ] **Step 8: Run the frontend test suite and type-check**

Run: `cd mur-hub-gui/ui && npx vitest run src/components/fleet/fleetCreateForm.test.ts`
Expected: PASS (4 describe blocks, all green).

Run: `cd mur-hub-gui/ui && npx tsc --noEmit`
Expected: no errors (confirms `en.ts`/`zh-TW.ts` key parity via `Record<TranslationKey, string>`, and that `FleetCreateModal.tsx` type-checks against `ModelOption`/`groupByProvider`).

- [ ] **Step 9: Commit**

```bash
git add mur-hub-gui/ui/src/components/fleet/FleetCreateModal.tsx mur-hub-gui/ui/src/components/fleet/fleetCreateForm.ts mur-hub-gui/ui/src/components/fleet/fleetCreateForm.test.ts mur-hub-gui/ui/src/styles/components/fleet.css mur-hub-gui/ui/src/i18n/en.ts mur-hub-gui/ui/src/i18n/zh-TW.ts
git commit -m "feat(hub): fleet creation Mode picker (plain/speculative/partition)"
```

---

## Task 6: FleetDetail Settings section

**Files:**
- Modify: `mur-hub-gui/ui/src/components/fleet/FleetDetail.tsx` (insert new section)
- Modify: `mur-hub-gui/ui/src/components/fleet/types.ts` (extend `FleetDetail`, add `FleetLoopView`)
- Modify: `mur-hub-gui/ui/src/styles/components/fleet.css` (append)
- Modify: `mur-hub-gui/ui/src/i18n/en.ts` and `zh-TW.ts` (append)

**Interfaces:**
- Consumes: Task 4's `fleet_set_loop` command and `FleetDetail.loop_cfg` field; Task 5's `DURATION_RE` from `./fleetCreateForm`.
- Produces: nothing other tasks depend on.

- [ ] **Step 1: Extend the TypeScript types**

In `mur-hub-gui/ui/src/components/fleet/types.ts`, add a new interface and extend `FleetDetail`:

```ts
export interface FleetLoopView {
  trigger: string;
  max_iterations: number;
  budget_usd: number;
  deadline: string;
  done_when: string;
  last_run: string | null;
}

export interface FleetDetail {
  name: string;
  display_name: string;
  goal: string;
  router: string;
  members: string[];
  channel_id: string;
  stopped: boolean;
  loop_cfg: FleetLoopView | null;
}
```

(This task only adds `loop_cfg` — Task 7 adds `parallel_summary` to this same interface. Both tasks touch this file; that's expected, they're sequential.)

- [ ] **Step 2: Write the failing test for trigger parsing**

There's no `FleetDetail.test.tsx` today. Following the same pure-function-extraction pattern as Task 5, create `mur-hub-gui/ui/src/components/fleet/fleetSettingsForm.test.ts` — note `fleetSettingsForm.ts` (the implementation it imports) does NOT exist yet, that's Step 4:

```ts
import { describe, it, expect } from "vitest";
import { parseTrigger, buildTrigger } from "./fleetSettingsForm";

describe("parseTrigger", () => {
  it("null loop_cfg → manual", () => {
    expect(parseTrigger(null)).toEqual({ kind: "manual", value: "" });
  });
  it("splits interval:<dur>", () => {
    expect(
      parseTrigger({ trigger: "interval:30m", max_iterations: 0, budget_usd: 0, deadline: "", done_when: "", last_run: null })
    ).toEqual({ kind: "interval", value: "30m" });
  });
  it("splits cron:<expr>", () => {
    expect(
      parseTrigger({ trigger: "cron:*/15 * * * *", max_iterations: 0, budget_usd: 0, deadline: "", done_when: "", last_run: null })
    ).toEqual({ kind: "cron", value: "*/15 * * * *" });
  });
});

describe("buildTrigger", () => {
  it("manual ignores value", () => {
    expect(buildTrigger("manual", "whatever")).toBe("manual");
  });
  it("interval/cron prepend the prefix and trim", () => {
    expect(buildTrigger("interval", " 30m ")).toBe("interval:30m");
    expect(buildTrigger("cron", "*/15 * * * *")).toBe("cron:*/15 * * * *");
  });
});
```

- [ ] **Step 3: Run it to confirm it fails**

Run: `cd mur-hub-gui/ui && npx vitest run src/components/fleet/fleetSettingsForm.test.ts`
Expected: FAIL — `Cannot find module './fleetSettingsForm'`.

- [ ] **Step 4: Implement `fleetSettingsForm.ts`**

Create `mur-hub-gui/ui/src/components/fleet/fleetSettingsForm.ts`:

```ts
/** Pure helpers for FleetDetail's Settings section. */

import type { FleetLoopView } from "./types";

export type TriggerKind = "manual" | "interval" | "cron";

export function parseTrigger(loopCfg: FleetLoopView | null): { kind: TriggerKind; value: string } {
  const trigger = loopCfg?.trigger ?? "manual";
  if (trigger.startsWith("interval:")) return { kind: "interval", value: trigger.slice("interval:".length) };
  if (trigger.startsWith("cron:")) return { kind: "cron", value: trigger.slice("cron:".length) };
  return { kind: "manual", value: "" };
}

export function buildTrigger(kind: TriggerKind, value: string): string {
  if (kind === "manual") return "manual";
  return `${kind}:${value.trim()}`;
}
```

- [ ] **Step 5: Run the test again, confirm it passes**

Run: `cd mur-hub-gui/ui && npx vitest run src/components/fleet/fleetSettingsForm.test.ts`
Expected: PASS.

- [ ] **Step 6: Insert the Settings section into `FleetDetail.tsx`**

Add to the import block at the top of `FleetDetail.tsx` (after the existing `import type { FleetDetail as Detail, JobRow } from "./types";`):

```tsx
import { DURATION_RE } from "./fleetCreateForm";
import { parseTrigger, buildTrigger, type TriggerKind } from "./fleetSettingsForm";
```

Inside the `FleetDetail` component function, after the existing state declarations (after `const [allJobs, setAllJobs] = useState<JobRow[]>([]);`), add:

```tsx
  const initialTrigger = parseTrigger(detail.loop_cfg);
  const [trigKind, setTrigKind] = useState<TriggerKind>(initialTrigger.kind);
  const [trigValue, setTrigValue] = useState(initialTrigger.value);
  const [maxIter, setMaxIter] = useState(
    detail.loop_cfg?.max_iterations ? String(detail.loop_cfg.max_iterations) : ""
  );
  const [deadline, setDeadlineValue] = useState(detail.loop_cfg?.deadline ?? "");
  const [budget, setBudget] = useState(
    detail.loop_cfg?.budget_usd ? String(detail.loop_cfg.budget_usd) : ""
  );
  const [doneWhen, setDoneWhen] = useState(detail.loop_cfg?.done_when ?? "");

  function settingsValid(): boolean {
    if (trigKind === "interval" && !DURATION_RE.test(trigValue.trim())) return false;
    if (trigKind === "cron" && trigValue.trim() === "") return false;
    if (deadline.trim() !== "" && !DURATION_RE.test(deadline.trim())) return false;
    return true;
  }

  const budgetWarning = trigKind !== "manual" && (!budget.trim() || Number(budget) <= 0);

  async function handleSaveSettings() {
    if (!settingsValid()) return;
    setBusy("fleet_set_loop");
    try {
      await invoke("fleet_set_loop", {
        name: detail.name,
        trigger: buildTrigger(trigKind, trigValue),
        maxIterations: maxIter.trim() ? Number(maxIter) : null,
        deadline: deadline.trim() || null,
        budgetUsd: budget.trim() ? Number(budget) : null,
        doneWhen: doneWhen.trim() || null,
      });
      showToast(t("fleet.settings.saved"));
      onRefresh();
    } catch (err) {
      showToast(String(err), 4000);
    } finally {
      setBusy(null);
    }
  }
```

Insert the JSX section right after the closing `</div>` of the Members section (after line 256, the `</div>` that closes `<div className="fleet-section">` containing Members — i.e. immediately before `<div className="fleet-section fleet-section--jobs">`):

```tsx
      <div className="fleet-section">
        <div className="fleet-section__label">{t("fleet.settings.title")}</div>
        <div className="fleet-settings__row">
          <label>{t("fleet.settings.trigger")}</label>
          <select value={trigKind} onChange={(e) => setTrigKind(e.target.value as TriggerKind)}>
            <option value="manual">{t("fleet.settings.triggerManual")}</option>
            <option value="interval">{t("fleet.settings.triggerInterval")}</option>
            <option value="cron">{t("fleet.settings.triggerCron")}</option>
          </select>
          {trigKind !== "manual" && (
            <input
              value={trigValue}
              onChange={(e) => setTrigValue(e.target.value)}
              placeholder={trigKind === "interval" ? "30m" : "*/15 * * * *"}
            />
          )}
        </div>
        {trigKind === "interval" && !DURATION_RE.test(trigValue.trim()) && (
          <div className="fleet-settings__warning">{t("fleet.settings.invalidDuration")}</div>
        )}
        <div className="fleet-settings__row">
          <label>{t("fleet.settings.maxIterations")}</label>
          <input value={maxIter} onChange={(e) => setMaxIter(e.target.value)} placeholder="8" />
        </div>
        <div className="fleet-settings__row">
          <label>{t("fleet.settings.deadline")}</label>
          <input value={deadline} onChange={(e) => setDeadlineValue(e.target.value)} placeholder="2h" />
        </div>
        {deadline.trim() !== "" && !DURATION_RE.test(deadline.trim()) && (
          <div className="fleet-settings__warning">{t("fleet.settings.invalidDuration")}</div>
        )}
        <div className="fleet-settings__hint">{t("fleet.settings.deadlineHint")}</div>
        <div className="fleet-settings__row">
          <label>{t("fleet.settings.budget")}</label>
          <input value={budget} onChange={(e) => setBudget(e.target.value)} placeholder="0.00" />
        </div>
        {budgetWarning && <div className="fleet-settings__warning">{t("fleet.settings.budgetWarning")}</div>}
        <div className="fleet-settings__row">
          <label>{t("fleet.settings.doneWhen")}</label>
          <input
            value={doneWhen}
            onChange={(e) => setDoneWhen(e.target.value)}
            placeholder={t("fleet.settings.doneWhenHint")}
          />
        </div>
        <div className="fleet-settings__hint">
          {t("fleet.settings.lastRun")}: {detail.loop_cfg?.last_run ?? t("fleet.settings.lastRunNever")}
          <br />
          {t("fleet.settings.stopHint")}
        </div>
        <button
          className="toolbar-btn toolbar-btn--primary"
          onClick={handleSaveSettings}
          disabled={busy !== null || !settingsValid()}
        >
          {t("fleet.settings.save")}
        </button>
      </div>
```

- [ ] **Step 7: Add the CSS**

Append to `fleet.css`:

```css
/* ── Detail: Settings section ──────────────────────────────────────────── */

.fleet-settings__row {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 8px;
  font-size: 12px;
}

.fleet-settings__row label {
  width: 110px;
  flex: none;
  color: var(--text-secondary);
}

.fleet-settings__row input,
.fleet-settings__row select {
  flex: 1;
  font-size: 12px;
  padding: 5px 8px;
}

.fleet-settings__warning {
  font-size: 11px;
  color: #d97706;
  margin: -4px 0 8px 118px;
}

.fleet-settings__hint {
  font-size: 11px;
  color: var(--text-tertiary);
  margin: 8px 0 12px;
}
```

- [ ] **Step 8: Add i18n keys**

In `en.ts`, after the keys Task 5 just added, append:

```ts
  "fleet.settings.title": "Settings",
  "fleet.settings.trigger": "Trigger",
  "fleet.settings.triggerManual": "Manual",
  "fleet.settings.triggerInterval": "Interval",
  "fleet.settings.triggerCron": "Cron",
  "fleet.settings.maxIterations": "Max iterations",
  "fleet.settings.deadline": "Deadline",
  "fleet.settings.deadlineHint": "e.g. 2h, 30m, 1d — relative to loop start, not a calendar date",
  "fleet.settings.budget": "Budget (USD)",
  "fleet.settings.budgetWarning": "Needed (> 0) for auto-run",
  "fleet.settings.doneWhen": "Done when",
  "fleet.settings.doneWhenHint": "marker:DONE (optional)",
  "fleet.settings.lastRun": "Last auto-run",
  "fleet.settings.lastRunNever": "never",
  "fleet.settings.stopHint": "Stopping this fleet (below) also blocks auto-run.",
  "fleet.settings.save": "Save Settings",
  "fleet.settings.saved": "Settings saved",
  "fleet.settings.invalidDuration": "Use a duration like 30s, 5m, 2h, 1d",
```

In `zh-TW.ts`, append:

```ts
  "fleet.settings.title": "設定",
  "fleet.settings.trigger": "觸發方式",
  "fleet.settings.triggerManual": "手動",
  "fleet.settings.triggerInterval": "間隔",
  "fleet.settings.triggerCron": "Cron",
  "fleet.settings.maxIterations": "最大迭代次數",
  "fleet.settings.deadline": "截止時間",
  "fleet.settings.deadlineHint": "例如 2h、30m、1d — 相對於迴圈開始的時間，非日曆日期",
  "fleet.settings.budget": "預算（美金）",
  "fleet.settings.budgetWarning": "自動執行需要大於 0 的預算",
  "fleet.settings.doneWhen": "完成條件",
  "fleet.settings.doneWhenHint": "marker:DONE（選填）",
  "fleet.settings.lastRun": "上次自動執行",
  "fleet.settings.lastRunNever": "從未執行",
  "fleet.settings.stopHint": "停止此機群（下方）也會阻止自動執行。",
  "fleet.settings.save": "儲存設定",
  "fleet.settings.saved": "設定已儲存",
  "fleet.settings.invalidDuration": "請使用如 30s、5m、2h、1d 的時間長度格式",
```

- [ ] **Step 9: Run tests and type-check**

Run: `cd mur-hub-gui/ui && npx vitest run src/components/fleet/fleetSettingsForm.test.ts`
Expected: PASS.

Run: `cd mur-hub-gui/ui && npx tsc --noEmit`
Expected: no errors. (`FleetDetail`'s `loop_cfg` field is used; `parallel_summary` referenced by Task 7 doesn't exist on the type yet until Task 7 runs — if Task 7 hasn't landed yet, `FleetDetail.tsx` must not reference `detail.parallel_summary` until Task 7 adds both the field and its usage together. This task's diff only touches `loop_cfg` — confirm no stray reference to `parallel_summary` was accidentally introduced.)

- [ ] **Step 10: Commit**

```bash
git add mur-hub-gui/ui/src/components/fleet/FleetDetail.tsx mur-hub-gui/ui/src/components/fleet/types.ts mur-hub-gui/ui/src/components/fleet/fleetSettingsForm.ts mur-hub-gui/ui/src/components/fleet/fleetSettingsForm.test.ts mur-hub-gui/ui/src/styles/components/fleet.css mur-hub-gui/ui/src/i18n/en.ts mur-hub-gui/ui/src/i18n/zh-TW.ts
git commit -m "feat(hub): fleet Settings section (trigger/budget/deadline/done_when)"
```

---

## Task 7: FleetDetail Run control redesign

**Files:**
- Modify: `mur-hub-gui/ui/src/components/fleet/FleetDetail.tsx`
- Modify: `mur-hub-gui/ui/src/components/fleet/types.ts` (add `ParallelSummary`, extend `FleetDetail`)
- Modify: `mur-hub-gui/ui/src/styles/components/fleet.css`
- Modify: `mur-hub-gui/ui/src/i18n/en.ts` and `zh-TW.ts`

**Interfaces:**
- Consumes: Task 4's `fleet_run(name, worktree, app)`, `fleet_run_loop(name, ...)`, `FleetDetail.parallel_summary`.
- Produces: nothing other tasks depend on.

- [ ] **Step 1: Extend the TypeScript types**

In `types.ts`, add (alongside `FleetLoopView` from Task 6):

```ts
export interface ParallelSummary {
  mode: "speculative" | "partition";
  track_count: number;
  target_file: string | null;
}
```

And extend `FleetDetail` (which Task 6 already modified to add `loop_cfg`) to also include:

```ts
export interface FleetDetail {
  name: string;
  display_name: string;
  goal: string;
  router: string;
  members: string[];
  channel_id: string;
  stopped: boolean;
  loop_cfg: FleetLoopView | null;
  parallel_summary: ParallelSummary | null;
}
```

- [ ] **Step 2: Write the failing test for the badge-label logic**

Add to `mur-hub-gui/ui/src/components/fleet/fleetSettingsForm.test.ts` (same file Task 6 created — this is small, cohesive logic about the same component, no need for a third file). `modeBadgeLabel` does NOT exist yet in `fleetSettingsForm.ts`, that's Step 4. Combine this import with the existing `parseTrigger, buildTrigger` import at the top of the test file into one statement from `"./fleetSettingsForm"`:

```ts
import { modeBadgeLabel } from "./fleetSettingsForm";

describe("modeBadgeLabel", () => {
  const t = (key: string) =>
    ({
      "fleet.create.mode.speculative": "Speculative",
      "fleet.create.mode.partition": "Partition",
      "fleet.run.tracksSuffix": "tracks",
    })[key] ?? key;

  it("null summary → null", () => {
    expect(modeBadgeLabel(null, t)).toBeNull();
  });
  it("speculative → mode · count tracks", () => {
    expect(modeBadgeLabel({ mode: "speculative", track_count: 2, target_file: null }, t)).toBe(
      "Speculative · 2 tracks"
    );
  });
  it("partition → mode · target_file", () => {
    expect(modeBadgeLabel({ mode: "partition", track_count: 0, target_file: "src/widget.rs" }, t)).toBe(
      "Partition · src/widget.rs"
    );
  });
});
```

- [ ] **Step 3: Run it to confirm it fails**

Run: `cd mur-hub-gui/ui && npx vitest run src/components/fleet/fleetSettingsForm.test.ts`
Expected: FAIL — `modeBadgeLabel` is not exported from `./fleetSettingsForm`.

- [ ] **Step 4: Implement `modeBadgeLabel`**

Add to `mur-hub-gui/ui/src/components/fleet/fleetSettingsForm.ts` (append to the existing file from Task 6 — add the import at the top alongside the existing `FleetLoopView` import, combining into one statement from `"./types"`):

```ts
import type { ParallelSummary } from "./types";

export function modeBadgeLabel(
  summary: ParallelSummary | null,
  t: (key: string, vars?: Record<string, string | number>) => string
): string | null {
  if (!summary) return null;
  if (summary.mode === "speculative") {
    return `${t("fleet.create.mode.speculative")} · ${summary.track_count} ${t("fleet.run.tracksSuffix")}`;
  }
  return `${t("fleet.create.mode.partition")} · ${summary.target_file ?? ""}`;
}
```

- [ ] **Step 5: Run the test again, confirm it passes**

Run: `cd mur-hub-gui/ui && npx vitest run src/components/fleet/fleetSettingsForm.test.ts`
Expected: PASS (now includes the `modeBadgeLabel` describe block alongside Task 6's `parseTrigger`/`buildTrigger` tests).

- [ ] **Step 6: Update `FleetDetail.tsx`'s run area and header**

Add to the imports: `import { parseTrigger, buildTrigger, modeBadgeLabel, type TriggerKind } from "./fleetSettingsForm";` (merge with Task 6's import line for this module — one import statement, all three names).

Add new state, alongside the Settings state Task 6 added:

```tsx
  const [worktree, setWorktree] = useState(false);
  const [loopOpen, setLoopOpen] = useState(false);
  const [loopIterations, setLoopIterations] = useState("");
  const [loopDeadline, setLoopDeadline] = useState("");
  const [loopBudget, setLoopBudget] = useState("");

  function toggleLoopPanel() {
    if (!loopOpen) {
      setLoopIterations(detail.loop_cfg?.max_iterations ? String(detail.loop_cfg.max_iterations) : "");
      setLoopDeadline(detail.loop_cfg?.deadline ?? "");
      setLoopBudget(detail.loop_cfg?.budget_usd ? String(detail.loop_cfg.budget_usd) : "");
    }
    setLoopOpen((v) => !v);
  }

  async function handleRunLoop() {
    showToast(t("fleet.runStarted"));
    await call("fleet_run_loop", {
      name: detail.name,
      maxIterations: loopIterations.trim() ? Number(loopIterations) : null,
      deadline: loopDeadline.trim() || null,
      budgetUsd: loopBudget.trim() ? Number(loopBudget) : null,
    });
  }
```

Replace `handleRun` (lines 61-64):

```tsx
  async function handleRun() {
    showToast(t("fleet.runStarted"));
    await call("fleet_run", { name: detail.name });
  }
```

with:

```tsx
  async function handleRun() {
    showToast(t("fleet.runStarted"));
    await call("fleet_run", { name: detail.name, worktree });
  }
```

Add the mode badge to the title row. Replace (lines 164-167):

```tsx
        <div className="fleet-detail__title-row">
          <h2 className="fleet-detail__title">{detail.display_name}</h2>
          <span className={statusPillClass(detail)}>{statusLabel(detail)}</span>
        </div>
```

with:

```tsx
        <div className="fleet-detail__title-row">
          <h2 className="fleet-detail__title">{detail.display_name}</h2>
          <span className={statusPillClass(detail)}>{statusLabel(detail)}</span>
          {modeBadgeLabel(detail.parallel_summary, t) && (
            <span className="fleet-detail__mode-badge">{modeBadgeLabel(detail.parallel_summary, t)}</span>
          )}
        </div>
```

Replace the entire Run control block (lines 172-177):

```tsx
      {/* Primary action */}
      <div className="fleet-detail__run">
        <button className="toolbar-btn toolbar-btn--primary" onClick={handleRun} disabled={busy !== null}>
          ▶ {t("fleet.run")}
        </button>
      </div>
```

with:

```tsx
      {/* Primary action */}
      <div className="fleet-detail__run">
        {detail.parallel_summary && (
          <label className="fleet-detail__worktree-toggle">
            <input
              type="checkbox"
              checked={worktree}
              onChange={(e) => setWorktree(e.target.checked)}
            />
            {t("fleet.run.worktree")}
          </label>
        )}
        <div className="fleet-detail__run-buttons">
          <button className="toolbar-btn toolbar-btn--primary" onClick={handleRun} disabled={busy !== null}>
            ▶ {t("fleet.run")}
          </button>
          <button className="toolbar-btn" onClick={toggleLoopPanel} disabled={busy !== null}>
            {t("fleet.run.loop")} {loopOpen ? "▴" : "▾"}
          </button>
        </div>
        {loopOpen && (
          <div className="fleet-detail__loop-row">
            <input
              value={loopIterations}
              onChange={(e) => setLoopIterations(e.target.value)}
              placeholder="8"
            />
            <input
              value={loopDeadline}
              onChange={(e) => setLoopDeadline(e.target.value)}
              placeholder="2h"
            />
            <input value={loopBudget} onChange={(e) => setLoopBudget(e.target.value)} placeholder="$" />
            <button
              className="toolbar-btn toolbar-btn--primary"
              onClick={handleRunLoop}
              disabled={busy !== null}
            >
              {t("fleet.run.go")}
            </button>
          </div>
        )}
      </div>
```

- [ ] **Step 7: Add the CSS**

Append to `fleet.css`:

```css
/* ── Detail: Mode badge + Run-as-loop ──────────────────────────────────── */

.fleet-detail__mode-badge {
  font-size: 11px;
  color: var(--text-tertiary);
  background: var(--bg-card, rgba(255,255,255,0.04));
  padding: 2px 8px;
  border-radius: 10px;
}

.fleet-detail__worktree-toggle {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  color: var(--text-secondary);
  margin-bottom: 8px;
  cursor: pointer;
}

.fleet-detail__run-buttons {
  display: flex;
  gap: 6px;
}

.fleet-detail__loop-row {
  display: flex;
  gap: 6px;
  margin-top: 8px;
}

.fleet-detail__loop-row input {
  width: 70px;
  font-size: 12px;
  padding: 6px 8px;
}
```

- [ ] **Step 8: Add i18n keys**

In `en.ts`, append:

```ts
  "fleet.run.worktree": "Use isolated worktrees (experimental)",
  "fleet.run.loop": "Run as loop",
  "fleet.run.go": "Go",
  "fleet.run.tracksSuffix": "tracks",
```

In `zh-TW.ts`, append:

```ts
  "fleet.run.worktree": "使用隔離工作樹（實驗性）",
  "fleet.run.loop": "以迴圈執行",
  "fleet.run.go": "開始",
  "fleet.run.tracksSuffix": "條軌道",
```

- [ ] **Step 9: Run tests and type-check**

Run: `cd mur-hub-gui/ui && npx vitest run src/components/fleet/fleetSettingsForm.test.ts`
Expected: PASS (now includes the `modeBadgeLabel` describe block).

Run: `cd mur-hub-gui/ui && npx tsc --noEmit`
Expected: no errors.

Run: `cd mur-hub-gui/ui && npx vitest run`
Expected: full frontend suite passes (confirms Tasks 5, 6, 7 together haven't broken anything else in the Hub UI).

- [ ] **Step 10: Commit**

```bash
git add mur-hub-gui/ui/src/components/fleet/FleetDetail.tsx mur-hub-gui/ui/src/components/fleet/types.ts mur-hub-gui/ui/src/components/fleet/fleetSettingsForm.ts mur-hub-gui/ui/src/components/fleet/fleetSettingsForm.test.ts mur-hub-gui/ui/src/styles/components/fleet.css mur-hub-gui/ui/src/i18n/en.ts mur-hub-gui/ui/src/i18n/zh-TW.ts
git commit -m "feat(hub): fleet Mode badge + Run-as-loop + isolation checkbox"
```

---

## Task 8: GeneralSettings auto-run toggle

**Files:**
- Modify: `mur-hub-gui/ui/src/components/settings/GeneralSettings.tsx`
- Modify: `mur-hub-gui/ui/src/i18n/en.ts` and `zh-TW.ts`

**Interfaces:**
- Consumes: Task 4's `get_fleet_autorun()` / `set_fleet_autorun(enabled)`.
- Produces: nothing other tasks depend on. Fully independent of Tasks 5-7 (different component tree entirely — `GeneralSettings` lives under the Settings modal, not the Fleets surface).

- [ ] **Step 1: Confirm current behavior, then write the component change directly**

This component is 53 lines with no existing test file and no business logic beyond two `<select>`s bound to local/localStorage state (see `GeneralSettings.tsx`, already read in full during planning) — there's no pure logic worth extracting into a separately-tested module here (a boolean toggle wired straight to two Tauri calls). Per the skill's "trivial one-liners need no test" allowance (and this project's established pattern of NOT unit-testing simple Tauri-bound UI toggles — `ModelsSettings.tsx`/`UpdatesSettings.tsx` etc. have no test files either), this task skips a dedicated test file and instead gets its correctness check from Step 3's manual verification. If you want a stricter TDD cycle here anyway, extract a one-line pure function `formatAutorunError(err: unknown): string` — but there's no actual logic to extract; the entire change is two `invoke()` calls and a checkbox. Proceed to Step 2.

- [ ] **Step 2: Add the toggle to `GeneralSettings.tsx`**

Replace the entire file:

```tsx
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../i18n";
import { applyTheme, getStoredTheme, type ThemeChoice } from "../../theme";

const THEMES: ThemeChoice[] = ["system", "light", "dark"];

function showToast(msg: string, durationMs = 2500) {
  const el = document.createElement("div");
  el.className = "toast";
  el.textContent = msg;
  document.body.appendChild(el);
  setTimeout(() => el.remove(), durationMs);
}

export function GeneralSettings() {
  const { t, lang, setLang } = useT();
  const [theme, setTheme] = useState<ThemeChoice>(getStoredTheme);
  const [fleetAutorun, setFleetAutorun] = useState(false);

  useEffect(() => {
    invoke<boolean>("get_fleet_autorun").then(setFleetAutorun).catch(() => {});
  }, []);

  async function handleFleetAutorunToggle(checked: boolean) {
    setFleetAutorun(checked);
    try {
      await invoke("set_fleet_autorun", { enabled: checked });
    } catch (err) {
      setFleetAutorun(!checked);
      showToast(String(err), 4000);
    }
  }

  return (
    <section className="settings-section">
      <h3 className="settings-section__title">{t("settings.nav.general")}</h3>

      <div className="settings-row">
        <label className="settings-row__label" htmlFor="settings-lang">
          {t("settings.language")}
        </label>
        <select
          id="settings-lang"
          className="input"
          value={lang}
          onChange={(e) => setLang(e.target.value as typeof lang)}
        >
          <option value="en">English</option>
          <option value="zh-TW">繁體中文</option>
        </select>
      </div>

      <div className="settings-row">
        <label className="settings-row__label" htmlFor="settings-theme">
          {t("settings.theme")}
        </label>
        <select
          id="settings-theme"
          className="input"
          value={theme}
          onChange={(e) => {
            const next = e.target.value as ThemeChoice;
            setTheme(next);
            applyTheme(next);
          }}
        >
          {THEMES.map((c) => (
            <option key={c} value={c}>
              {t(`settings.theme.${c}` as Parameters<typeof t>[0])}
            </option>
          ))}
        </select>
      </div>

      <div className="settings-row">
        <label className="settings-row__label" htmlFor="settings-fleet-autorun">
          {t("settings.fleetAutorun.label")}
        </label>
        <input
          id="settings-fleet-autorun"
          type="checkbox"
          checked={fleetAutorun}
          onChange={(e) => handleFleetAutorunToggle(e.target.checked)}
        />
      </div>
      <p className="settings-row__hint">{t("settings.fleetAutorun.description")}</p>
    </section>
  );
}
```

(`.settings-row__hint` is a new class — check `mur-hub-gui/ui/src/styles/components/settings.css` or equivalent for whether this class already exists from another settings row; if it doesn't exist, add it following whatever convention `.settings-row__label` uses in that file, e.g. `font-size: 11px; color: var(--text-tertiary); margin: -6px 0 12px;`.)

- [ ] **Step 3: Manual verification (build + Computer Use)**

This toggle has no automated test (Step 1 rationale). Verify manually once the Hub `.app` is built (see Task 9-equivalent manual verification pass at the end of this plan — don't do a separate build here, fold this check into that pass): open Settings → General, confirm the checkbox reflects `~/.mur/config.yaml`'s `fleet.autorun` value on load, toggle it, confirm the file updates.

- [ ] **Step 4: Add i18n keys**

In `en.ts`, append:

```ts
  "settings.fleetAutorun.label": "Allow fleets to auto-run unattended",
  "settings.fleetAutorun.description": "Fleets with a trigger and budget configured will run on schedule without confirmation. Off by default. Requires per-fleet budget > 0.",
```

In `zh-TW.ts`, append:

```ts
  "settings.fleetAutorun.label": "允許機群無人值守自動執行",
  "settings.fleetAutorun.description": "已設定觸發條件與預算的機群將依排程自動執行，無需確認。預設關閉，且每個機群的預算必須大於 0。",
```

- [ ] **Step 5: Type-check**

Run: `cd mur-hub-gui/ui && npx tsc --noEmit`
Expected: no errors.

- [ ] **Step 6: Commit**

```bash
git add mur-hub-gui/ui/src/components/settings/GeneralSettings.tsx mur-hub-gui/ui/src/i18n/en.ts mur-hub-gui/ui/src/i18n/zh-TW.ts
git commit -m "feat(hub): global 'allow fleets to auto-run unattended' settings toggle"
```

---

## Final manual verification pass (after all 8 tasks)

Not a task with its own commit — a Computer-Use pass over the whole feature, matching the spec's Testing section and the original Fleet Surface spec's verification style.

- [ ] Build the Hub `.app` locally (see `gotcha_hub_local_app_build_recipe` if available, or `build.sh`/`cargo tauri build` per `mur-hub-gui/README.md`).
- [ ] Create a Plain fleet via the modal — confirm it behaves identically to before this plan (regression check on the 4 original fields).
- [ ] Create a Speculative fleet (2 tracks, judge model, one pre-filter checked) — inspect `~/.mur/fleets/<name>/fleet.yaml` on disk, confirm the `parallel:` block matches what was entered.
- [ ] Create a Partition fleet (target file) — same disk check.
- [ ] Open an existing fleet's Settings, set `Interval` trigger to `30m` and budget to `5`, Save — confirm `loop:` block on disk; confirm the budget-warning message appears when trigger≠Manual and budget is 0, and disappears once a positive budget is entered; confirm an invalid duration (e.g. typing `2026-12-31` into Deadline) disables Save and shows the inline error.
- [ ] Click Run on a Plain fleet — confirm unchanged one-shot behavior (toast, job/list refresh on `fleet:run_done`).
- [ ] Click "Run as loop ▾" → fill small overrides → Go — confirm a loop run starts (check process/logs for `cmd_fleet_run_loop`), `fleet:run_done` eventually fires, jobs refresh.
- [ ] On a Speculative fleet, check "Use isolated worktrees", click Run — confirm `.worktrees/` appears under the repo root with one worktree per track (cross-check against the existing Tier-1 dogfood verification approach from `project_parallel_tracks_p3_concurrent_merge.md`).
- [ ] Confirm the isolation checkbox does NOT appear at all for a Plain fleet, and does NOT appear anywhere near the "Run as loop" controls for any fleet.
- [ ] In Settings → General, toggle "Allow fleets to auto-run unattended" on; on a fleet with `interval:1m` (temporarily, for fast verification) + a budget, wait ~90s with the daemon running and `MUR_FLEET_AUTORUN` unset — confirm the daemon auto-runs it (check `.last_run` timestamp advances, or daemon logs for "fleet_tick: auto-running loop").
- [ ] Toggle the global switch back off (env var still unset) — confirm the same fleet no longer auto-fires on the next interval.
- [ ] Confirm the existing CLI still works unchanged for everything not touched by this plan (`mur fleet list`, `show`, `stop`/`start`, `export`/`import`, `add`/`remove`, `compare`/`judge`/`cherry`/`partition-plan`/`merge`/`merge-concurrent`).

## After verification: open a PR

Per the user's instruction, once implementation is complete and manually verified, open a pull request (don't push straight to `main`). Follow the repo's standard PR flow: push the feature branch, `gh pr create` with a summary of the 8 tasks and a test plan checklist mirroring the manual verification pass above.
