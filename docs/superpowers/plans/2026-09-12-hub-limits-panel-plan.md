# Plan: Hub 3b — the `LimitsPanel`, rendered at three scopes

> Execute with **`mur-executing-plans`**. Spec:
> `docs/superpowers/specs/2026-09-12-execution-limits-design.md` §10 (10.1–10.5), D4, §5.
> Base: `main` at v2.80.0 (steps 1–6 shipped).

**Goal.** The Hub shows every execution bound in force — with the scope that set it — at the global Settings page, the fleet Settings tab and the agent Overview, lets the user edit exactly one scope, shows the §5 bounded badge on the fleet, and shows the stop reason where the Hub user is looking.

**Architecture.** One rule: **the Hub renders what the CLI resolves.** `mur_core::cmd::limits::report` grows the fields the Hub needs (`applies`, `local`, `raw`, structured stale rows, `billable`, a `Global` target) so nothing is re-derived in TypeScript. The Tauri side adds one module, `limits.rs`, with three commands — `limits_resolve`, `limits_set`, `limits_remove_stale` — that wrap `cmd::limits` / `cmd::limits_write`, and `fleet_detail` embeds the fleet's `LimitsView`. The UI adds one component, `LimitsPanel`, driven by a pure helper module with tests, and mounts it at the three scopes; `FleetSettings` loses the guard inputs, `FleetOverview`'s cards change, `FleetHeader` gets the badge, and job rows carry the channel's `stop_reason`.

**Tech stack.** Rust 2024 (`mur-core`, workspace-excluded `mur-hub-gui/src-tauri`), React + TypeScript + vitest (`mur-hub-gui/ui`). `mur-core` env: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432`; from a worktree add `CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target`. Hub Rust: `cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml` needs `mur-hub-gui/ui/dist` (symlink it from the main checkout in a worktree; remove before committing) and `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml --lib`. UI: `cd mur-hub-gui/ui && npx vitest run` and `npx tsc --noEmit`.

## Global Constraints (from the spec)

- §10: **the Hub renders what the CLI resolves.** `limits_resolve` wraps the same resolver `mur limits` uses and returns per knob `{value, source, applies, note}`; the Hub never re-derives a default or an applicability rule in TypeScript.
- §10.1: each row is `knob · effective value · source chip · action`. Inherited → dimmed, chip names the source, action *Override here*. Local → solid, action *Reset to inherited*, which **deletes the key** (never writes the parent's value). `cost_usd` is an editable row only when `applies == true`; otherwise one line of text naming why — **never a disabled input**. Duration validation is the CLI parser, exposed through the command's error.
- §10.2: fleet Overview cards become `Last auto-run · Deadline · Stuck · Done when`; on a billable fleet with a cap the third card is `$5.00 / Cost cap`. Header badge: `bounded ✓` / `bounded by deadline only · no cost cap` (amber) / `unbounded — will not auto-run` (links to Settings).
- §10.3: job rows show `■ stopped: <reason>` from the channel `state-change` event with one inline action that opens the panel at the fleet scope with that knob focused; `finished` only for `converged`.
- §10.4: stale keys are amber rows at the top with *Remove*; the agent panel says a save needs a restart; the Hub chat pane is attended (out of scope here — already true since step 4).
- §10.5: exactly three commands; `fleet_set_loop` keeps trigger / cron / done-when and drops the guard fields; `LimitsView` is a DTO owned by the Tauri side.
- The Hub is workspace-excluded: every `pub fn` signature or `mur-common` struct this plan changes must be grepped in `mur-hub-gui/src-tauri/src`; run the Hub `cargo check` and `cargo test --lib` last in every task that touches Rust.
- Every new user-visible string gets an `en.ts` key **and** a `zh-TW.ts` entry (the runtime falls back to `en`, but zh-TW is the user's language — do not leave it English).
- Before every commit: `cargo fmt`, `cargo clippy -p mur-core --all-targets -- -D warnings` (exit code), Hub clippy needs `ui/dist`, `npx vitest run`, `npx tsc --noEmit`.

## File structure

| File | Responsibility | Task |
|---|---|---|
| `mur-core/src/cmd/limits.rs` | `Row { applies, local, raw }`, `Stale { key, file, value, message }`, `LimitsReport { billable, needs_restart }`, `Target::Global`, `render_json` carries them; tests | 1 |
| `mur-core/src/cmd/limits_write.rs` | `remove_stale_key(mur_home, &Target, key)`; test | 1 |
| `mur-hub-gui/src-tauri/src/limits.rs` (new) | DTOs `LimitsView`, `LimitsRowView`, `StaleView`; `limits_resolve`, `limits_set`, `limits_remove_stale`; `bounded_of`; tests | 2 |
| `mur-hub-gui/src-tauri/src/lib.rs` | `mod limits;` + three commands registered | 2 |
| `mur-hub-gui/src-tauri/src/fleet.rs` | `FleetLoopView` loses `max_iterations`/`budget_usd`; `FleetDetail.limits`, `FleetDetail.last_stop`; `JobRow.stop_reason/remedy`; `fleet_set_loop` and `fleet_run_loop` drop the guard args | 2, 5 |
| `mur-hub-gui/ui/src/components/fleet/types.ts` | `LimitsView`, `LimitsRowView`, `StaleView`, `StopInfo`; `FleetLoopView`/`FleetDetail`/`JobRow` updated | 3, 5 |
| `mur-hub-gui/ui/src/components/limits/limitsPanel.ts` (new) + `.test.ts` | pure: row presentation, action, badge, patch building | 3 |
| `mur-hub-gui/ui/src/components/limits/LimitsPanel.tsx` (new) | the component: rows, stale rows, edit/override/reset, restart note | 3 |
| `mur-hub-gui/ui/src/components/settings/GeneralSettings.tsx` | "Execution limits" section → `<LimitsPanel scope="global" />` | 3 |
| `mur-hub-gui/ui/src/components/detail/fleet/FleetSettings.tsx`, `../../fleet/fleetSettingsForm.ts` (+test) | guard inputs removed; panel mounted; `settingsAreValid(trigKind, trigValue)` | 3 |
| `mur-hub-gui/ui/src/components/detail/agent/OverviewTab.tsx` | "Limits" card with Edit → panel | 3 |
| `mur-hub-gui/ui/src/components/detail/fleet/FleetOverview.tsx`, `FleetHeader.tsx` | stat cards, badge, last-stop row | 4 |
| `mur-hub-gui/ui/src/components/detail/fleet/FleetJobs.tsx`, `FleetHost.tsx` | `stopped: <reason>` status + action that opens Settings with the knob focused | 5 |
| `mur-hub-gui/ui/src/i18n/en.ts`, `zh-TW.ts` | `limits.*` keys | 3–5 |
| `mur-hub-gui/ui/src/styles/*.css` | `.limits-row`, `.limits-row--inherited`, `.limits-chip`, `.limits-stale`, `.fleet-detail__bounded--{ok,amber,off}` | 3, 4 |

---

## Task 1 — the CLI report carries what a panel needs

**Interfaces.**
- Consumes: `cmd::limits::{Target, Row, LimitsReport, report, render_json}`, `cmd::limits_write::{Patch, apply_patch, write_fleet_limits, write_agent_limits, upsert_global_limits}` (step 3), `Fleet.limits`, `AgentProfile.{limits, hitl}`, `Config.limits`.
- Produces (all in `mur-core/src/cmd/limits.rs` unless noted):

```rust
pub enum Target { Global, Fleet(String), Agent(String) }
pub struct Row {
    pub knob: &'static str,
    pub value: String,          // as printed today ("2h", "$5.00", "off", "—")
    pub source: String,         // as printed today; "" when !applies
    pub note: Option<String>,
    pub applies: bool,          // false only for cost_usd on a non-billable scope
    pub local: bool,            // the queried scope itself set it (source == that scope)
    pub raw: Option<String>,    // the string a user edits: "2h" / "off" / "5" / None when inherited
}
pub struct Stale { pub key: String, pub file: String, pub value: String, pub message: String }
pub struct LimitsReport {
    pub target: Target,
    pub rows: Vec<Row>,
    pub stale: Vec<Stale>,
    pub attended_note: &'static str,
    pub billable: bool,         // Global: true (unknown model → metered, same rule as unknown billing)
    pub needs_restart: bool,    // Agent only
}
// render_json adds: rows[*].applies/local/raw, stale[*] as objects, billable, needs_restart, target.kind "global"
// cmd/limits_write.rs
pub fn remove_stale_key(mur_home: &Path, target: &Target, key: &str) -> anyhow::Result<()>
//   fleet: "loop.max_iterations" → 0, "loop.budget_usd" → 0.0, "loop.deadline" → ""
//   agent: "hitl.max_iterations" / "hitl.max_tokens" → None ; anything else → Err naming the allowed keys
```

  `render_human` keeps its output byte-for-byte (`stale:` lines print `Stale.message`) — the step-3 tests stay green.

### Steps

- [x] **1.1 Write the failing tests** — in `mur-core/src/cmd/limits.rs` tests:

```rust
    /// The fields a panel needs and the CLI never printed: is the row
    /// editable here, did THIS scope set it, and what string does the user
    /// edit. A fleet that set deadline has local=true and raw="2h"; the stuck
    /// row it inherited has local=false and raw=None; cost on a local fleet
    /// has applies=false.
    #[test]
    fn rows_say_local_applies_and_raw() {
        let f = dev(None, Some(mur_common::limits::Limits { deadline: Some("2h".into()), stuck: None, cost_usd: None }));
        let t = home_with_fleet(&f);
        std::fs::write(t.path().join("models.yaml"), "models:\n  local:\n    provider: openai\n    model: q\n    billing: local\n").unwrap();
        std::fs::create_dir_all(t.path().join("agents/pm")).unwrap();
        std::fs::write(t.path().join("agents/pm/profile.yaml"), "name: pm\nmodel_ref: local\n").unwrap();
        let r = report(t.path(), &Target::Fleet("dev".into())).unwrap();
        let by = |k: &str| r.rows.iter().find(|row| row.knob == k).unwrap();
        assert!(by("deadline").local && by("deadline").raw.as_deref() == Some("2h"));
        assert!(!by("stuck").local && by("stuck").raw.is_none());
        assert!(!by("cost_usd").applies && !r.billable);
        let j = render_json(&r);
        assert_eq!(j["rows"][0]["local"], true);
        assert_eq!(j["billable"], false);
        assert_eq!(j["needs_restart"], false);
    }

    /// Stale keys are structured so a panel can offer Remove per key.
    #[test]
    fn stale_keys_are_structured() {
        let f = dev(Some(FleetLoop { trigger: "manual".into(), max_iterations: 8, budget_usd: 0.0, deadline: String::new(), done_when: String::new() }), None);
        let t = home_with_fleet(&f);
        let r = report(t.path(), &Target::Fleet("dev".into())).unwrap();
        assert_eq!(r.stale[0].key, "loop.max_iterations");
        assert_eq!(r.stale[0].file, "fleet.yaml");
        assert_eq!(r.stale[0].value, "8");
        assert!(r.stale[0].message.contains("IGNORED since 2.79"));
        let j = render_json(&r);
        assert_eq!(j["stale"][0]["key"], "loop.max_iterations");
    }

    /// The global scope reports config.yaml's own values: what it set is
    /// local there, the rest is built-in; nothing is inherited from above.
    #[test]
    fn global_target_reports_config_yaml() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("config.yaml"), "limits:\n  stuck: 20m\n").unwrap();
        let r = report(t.path(), &Target::Global).unwrap();
        let by = |k: &str| r.rows.iter().find(|row| row.knob == k).unwrap();
        assert_eq!(by("stuck").source, "~/.mur/config.yaml");
        assert!(by("stuck").local);
        assert_eq!(by("deadline").source, "built-in default");
        assert!(by("cost_usd").applies, "global cost cap applies to whatever metered model runs under it");
        assert_eq!(render_json(&r)["target"]["kind"], "global");
    }

    #[test]
    fn agent_target_needs_restart() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("agents/pm")).unwrap();
        std::fs::write(t.path().join("agents/pm/profile.yaml"), "name: pm\nhitl:\n  max_iterations: 800\n").unwrap();
        let r = report(t.path(), &Target::Agent("pm".into())).unwrap();
        assert!(r.needs_restart);
        assert_eq!(r.stale[0].key, "hitl.max_iterations");
        assert_eq!(r.stale[0].value, "800");
    }
```

  (`dev`/`home_with_fleet` are the module's existing fixtures; add `needs: vec![]` to `dev` if the literal lacks it.)

  and in `mur-core/src/cmd/limits_write.rs` tests:

```rust
    #[test]
    fn remove_stale_key_clears_exactly_that_key() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path();
        let mut f = fleet_named_for_test("dev");   // reuse the module's fixture or add one mirroring cmd/limits.rs::dev
        f.loop_cfg = Some(mur_common::fleet::FleetLoop { trigger: "manual".into(), max_iterations: 8, budget_usd: 5.0, deadline: "2h".into(), done_when: String::new() });
        crate::cmd::fleet::store::save_fleet(home, &f).unwrap();
        remove_stale_key(home, &crate::cmd::limits::Target::Fleet("dev".into()), "loop.max_iterations").unwrap();
        let back = crate::cmd::fleet::store::load_fleet(home, "dev").unwrap();
        assert_eq!(back.loop_cfg.as_ref().unwrap().max_iterations, 0);
        assert_eq!(back.loop_cfg.as_ref().unwrap().budget_usd, 5.0, "the other keys survive");
        std::fs::create_dir_all(home.join("agents/pm")).unwrap();
        std::fs::write(home.join("agents/pm/profile.yaml"), "name: pm\nhitl:\n  max_iterations: 800\n  max_tokens: 9\n").unwrap();
        unsafe { std::env::set_var("MUR_HOME", home) };
        remove_stale_key(home, &crate::cmd::limits::Target::Agent("pm".into()), "hitl.max_tokens").unwrap();
        unsafe { std::env::remove_var("MUR_HOME") };
        let p = mur_common::agent::AgentProfile::load(home, "pm").unwrap();
        assert_eq!(p.hitl.max_tokens, None);
        assert_eq!(p.hitl.max_iterations, Some(800));
        let e = remove_stale_key(home, &crate::cmd::limits::Target::Fleet("dev".into()), "loop.trigger").unwrap_err().to_string();
        assert!(e.contains("loop.max_iterations"), "names the allowed keys: {e}");
    }
```

  (`write_agent_limits`/`save_profile` resolve the home from `MUR_HOME` — the test sets it, as the step-3 CLI tests do; nextest runs each test in its own process.)

- [x] **1.2 Watch them fail** — `cargo nextest run -p mur-core --lib -E 'test(/cmd::limits/)'`.

- [x] **1.3 Implement** — `Row` gains `applies: bool, local: bool, raw: Option<String>`; `rows_from` takes the queried scope's `Source` (`Source::Fleet` / `Source::Agent` / `Source::Global`) and the scope's own `Limits` (to fill `raw`: `deadline.clone()`, `stuck.clone()`, `cost_usd.map(|c| format!("{c}"))`); `local = r.<knob>.source == scope_source`; `applies = cost_applies.is_none()`. `Stale` replaces `Vec<String>`: build each with `key`, `file` (`"fleet.yaml"` / `"profile.yaml"`), `value`, `message` (the current string). `render_human` prints `stale: {message}` unchanged. `LimitsReport.billable` = `billing.billable` (fleet) / `matches!(billing, Some(UsageBilled) | None)` (agent) / `true` (global); `needs_restart = matches!(target, Target::Agent(_))`. `Target::Global`: `resolve(Scope::FleetRun, &global, None, None, &none)` with `scope_source = Source::Global`, `cost_applies = None`, no stale, `attended_note` as the fleet's. `render_json`: `"target": {"kind": "global"|"fleet"|"agent", "name": name_or_null}`, rows with the three new fields, `"stale": [{key,file,value,message}]`, `"billable"`, `"needs_restart"`. `detect_target` is unchanged (a name is never global). `cmd_limits --global` (dispatch) keeps printing the block.

  `remove_stale_key` in `limits_write.rs`:

```rust
/// Delete one legacy key through the same store the edits use (spec §10.4):
/// the fleet's loop.* through save_fleet, the agent's hitl.* through
/// save_profile. Anything else is refused by name so a typo cannot clear a
/// live setting.
pub fn remove_stale_key(mur_home: &Path, target: &crate::cmd::limits::Target, key: &str) -> Result<()> {
    use crate::cmd::limits::Target;
    match (target, key) {
        (Target::Fleet(name), "loop.max_iterations" | "loop.budget_usd" | "loop.deadline") => {
            let mut fleet = crate::cmd::fleet::store::load_fleet(mur_home, name)?;
            if let Some(lc) = fleet.loop_cfg.as_mut() {
                match key {
                    "loop.max_iterations" => lc.max_iterations = 0,
                    "loop.budget_usd" => lc.budget_usd = 0.0,
                    _ => lc.deadline.clear(),
                }
            }
            crate::cmd::fleet::store::save_fleet(mur_home, &fleet)
        }
        (Target::Agent(name), "hitl.max_iterations" | "hitl.max_tokens") => {
            let (path, mut profile) = crate::cmd::agent::load_profile_for_edit(name)?;
            if key == "hitl.max_iterations" { profile.hitl.max_iterations = None } else { profile.hitl.max_tokens = None }
            crate::cmd::agent::save_profile(&path, &mut profile)
        }
        (Target::Global, _) => anyhow::bail!("config.yaml has no stale limits keys"),
        (Target::Fleet(_), other) => anyhow::bail!("`{other}` is not a stale fleet key (loop.max_iterations, loop.budget_usd, loop.deadline)"),
        (Target::Agent(_), other) => anyhow::bail!("`{other}` is not a stale agent key (hitl.max_iterations, hitl.max_tokens)"),
    }
}
```

- [x] **1.4 Watch it pass** — `cargo nextest run -p mur-core --lib -E 'test(/cmd::limits/)'`; `command grep -rn "cmd::limits::\|limits_write::" mur-hub-gui/src-tauri/src` must print nothing yet (the Hub does not call these before Task 2).

- [x] **1.5 fmt + clippy on `mur-core`**, then **commit**:

```
feat(limits): the report carries applies / local / raw, structured stale keys, a Global target

What a panel needs and the CLI never printed. render_human is unchanged;
render_json grows the fields. remove_stale_key deletes one legacy key
through the same store the edits use.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 2 — three Tauri commands, and the fleet detail carries its limits

**Interfaces.**
- Consumes: Task 1; `mur_home_path()` (`lib.rs`); `settings::cmd_fleet_set_loop`; `loop_run::cmd_fleet_run_loop`.
- Produces (in `mur-hub-gui/src-tauri/src/limits.rs`):

```rust
#[derive(Serialize, Clone)] pub struct LimitsRowView { pub knob: String, pub value: String, pub source: String, pub note: Option<String>, pub applies: bool, pub local: bool, pub raw: Option<String> }
#[derive(Serialize, Clone)] pub struct StaleView { pub key: String, pub file: String, pub value: String, pub message: String }
#[derive(Serialize, Clone)] pub struct LimitsView {
    pub scope: String,            // "global" | "fleet" | "agent"
    pub name: Option<String>,
    pub rows: Vec<LimitsRowView>,
    pub stale: Vec<StaleView>,
    pub billable: bool,
    pub needs_restart: bool,
    pub attended_note: String,
    pub bounded: String,          // "bounded" | "deadline_only" | "unbounded"
    pub error: Option<String>,    // set when the limits: block does not resolve; rows empty, bounded = unbounded
}
#[derive(Deserialize)] pub struct LimitsPatchArg { pub deadline: Option<String>, pub stuck: Option<String>, pub cost_usd: Option<f64>, #[serde(default)] pub unset: Vec<String> }
#[tauri::command] pub fn limits_resolve(scope: String, name: Option<String>) -> Result<LimitsView, String>
#[tauri::command] pub fn limits_set(scope: String, name: Option<String>, patch: LimitsPatchArg) -> Result<LimitsView, String>   // writes, then resolves again
#[tauri::command] pub fn limits_remove_stale(scope: String, name: Option<String>, key: String) -> Result<LimitsView, String>
pub(crate) fn resolve_in(home: &Path, scope: &str, name: Option<&str>) -> LimitsView          // testable core
pub(crate) fn bounded_of(report: &LimitsReport) -> &'static str
```

  `fleet.rs`: `FleetDetail.limits: LimitsView`; `FleetLoopView { trigger, deadline, done_when, last_run }` (guards gone — `deadline` stays only as the legacy string the panel's stale row points at); `fleet_set_loop(name, trigger, done_when)`; `fleet_run_loop(name, app)`.

### Steps

- [x] **2.1 Write the failing tests** — `mur-hub-gui/src-tauri/src/limits.rs` tests (the Tauri crate's tests use `tempfile`; mirror `fleet.rs`'s test module):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn fleet_home(deadline: Option<&str>, billing: &str) -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        let h = t.path();
        std::fs::create_dir_all(h.join("fleets/dev")).unwrap();
        let limits = deadline.map(|d| format!("limits:\n  deadline: {d}\n")).unwrap_or_default();
        std::fs::write(h.join("fleets/dev/fleet.yaml"), format!("name: dev\ngoal: g\nchannel_id: fleet-dev\nmembers: [pm]\n{limits}")).unwrap();
        std::fs::create_dir_all(h.join("agents/pm")).unwrap();
        std::fs::write(h.join("agents/pm/profile.yaml"), "name: pm\nmodel_ref: m\n").unwrap();
        std::fs::write(h.join("models.yaml"), format!("models:\n  m:\n    provider: openai\n    model: x\n    billing: {billing}\n")).unwrap();
        t
    }

    /// §5 made visible: a local fleet is bounded by its (built-in) deadline;
    /// a billable fleet with no cap is bounded by deadline only — amber; a
    /// block that does not parse is unbounded and the view says why.
    #[test]
    fn bounded_badge_follows_the_resolver() {
        let t = fleet_home(None, "local");
        let v = resolve_in(t.path(), "fleet", Some("dev"));
        assert_eq!(v.bounded, "bounded");
        assert!(!v.billable);
        let t = fleet_home(Some("2h"), "usage_billed");
        let v = resolve_in(t.path(), "fleet", Some("dev"));
        assert_eq!(v.bounded, "deadline_only");
        assert!(v.rows.iter().any(|r| r.knob == "cost_usd" && r.applies && r.value == "—"));
        let t = fleet_home(Some("soon"), "local");
        let v = resolve_in(t.path(), "fleet", Some("dev"));
        assert_eq!(v.bounded, "unbounded");
        assert!(v.error.as_deref().unwrap_or("").contains("limits.deadline"));
        assert!(v.rows.is_empty());
    }

    /// set writes ONE scope and returns the fresh view; unset deletes the key
    /// so the row goes back to inherited (local=false).
    #[test]
    fn set_and_unset_round_trip_through_the_view() {
        let t = fleet_home(None, "local");
        let v = set_in(t.path(), "fleet", Some("dev"), LimitsPatchArg { deadline: Some("3h".into()), stuck: None, cost_usd: None, unset: vec![] }).unwrap();
        let d = v.rows.iter().find(|r| r.knob == "deadline").unwrap();
        assert!(d.local && d.raw.as_deref() == Some("3h") && d.value == "3h");
        let v = set_in(t.path(), "fleet", Some("dev"), LimitsPatchArg { deadline: None, stuck: None, cost_usd: None, unset: vec!["deadline".into()] }).unwrap();
        let d = v.rows.iter().find(|r| r.knob == "deadline").unwrap();
        assert!(!d.local && d.source == "built-in default");
        let e = set_in(t.path(), "fleet", Some("dev"), LimitsPatchArg { deadline: Some("soon".into()), stuck: None, cost_usd: None, unset: vec![] }).unwrap_err();
        assert!(e.contains("limits.deadline"), "the CLI parser's words: {e}");
    }

    #[test]
    fn global_scope_edits_config_yaml_as_text() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("config.yaml"), "research_gateway:\n  key: k\n").unwrap();
        let v = set_in(t.path(), "global", None, LimitsPatchArg { deadline: None, stuck: Some("15m".into()), cost_usd: None, unset: vec![] }).unwrap();
        assert!(v.rows.iter().any(|r| r.knob == "stuck" && r.local && r.value == "15m"));
        let text = std::fs::read_to_string(t.path().join("config.yaml")).unwrap();
        assert!(text.contains("research_gateway:\n  key: k\n") && text.contains("limits:\n  stuck: 15m\n"), "{text}");
    }
}
```

  (`set_in(home, scope, name, patch) -> Result<LimitsView, String>` is the testable core of `limits_set`; the agent scope's write goes through `MUR_HOME` — the Hub already runs with the same home, and the test for agent scope is the CLI's.)

- [x] **2.2 Watch them fail** — `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml --lib limits::` (symlink `ui/dist` first).

- [x] **2.3 Implement** — `limits.rs`:

```rust
//! The Hub's limits surface (spec 2026-09-12 §10): three commands over the
//! same resolver `mur limits` uses. Nothing here decides a default or an
//! applicability rule — the report does, and this file only reshapes it.

use std::path::Path;

use mur_core::cmd::limits::{LimitsReport, Target, report};
use mur_core::cmd::limits_write::{Patch, remove_stale_key, upsert_global_limits, write_agent_limits, write_fleet_limits};
use serde::{Deserialize, Serialize};

// … DTOs as in Interfaces …

fn target_of(scope: &str, name: Option<&str>) -> Result<Target, String> {
    match (scope, name) {
        ("global", _) => Ok(Target::Global),
        ("fleet", Some(n)) => Ok(Target::Fleet(n.to_string())),
        ("agent", Some(n)) => Ok(Target::Agent(n.to_string())),
        _ => Err(format!("limits: scope `{scope}` needs a name")),
    }
}

/// §5 as a word: the resolver's deadline bounds every scope; a billable one
/// without a cost cap is bounded by the deadline ONLY (amber); a report that
/// failed to resolve is unbounded.
pub(crate) fn bounded_of(r: &LimitsReport) -> &'static str {
    let cost = r.rows.iter().find(|row| row.knob == "cost_usd");
    match cost {
        Some(c) if r.billable && c.applies && c.value == "—" => "deadline_only",
        _ => "bounded",
    }
}

fn view_of(scope: &str, name: Option<&str>, r: LimitsReport) -> LimitsView { /* map fields 1:1; bounded = bounded_of(&r) */ }

fn error_view(scope: &str, name: Option<&str>, e: String) -> LimitsView {
    LimitsView { scope: scope.into(), name: name.map(str::to_string), rows: vec![], stale: vec![], billable: true, needs_restart: false, attended_note: String::new(), bounded: "unbounded".into(), error: Some(e) }
}

pub(crate) fn resolve_in(home: &Path, scope: &str, name: Option<&str>) -> LimitsView {
    match target_of(scope, name).and_then(|t| report(home, &t).map_err(|e| format!("{e:#}"))) {
        Ok(r) => view_of(scope, name, r),
        Err(e) => error_view(scope, name, e),
    }
}

pub(crate) fn set_in(home: &Path, scope: &str, name: Option<&str>, patch: LimitsPatchArg) -> Result<LimitsView, String> {
    let p = Patch { deadline: patch.deadline, stuck: patch.stuck, cost_usd: patch.cost_usd, unset: patch.unset };
    match target_of(scope, name)? {
        Target::Global => upsert_global_limits(&home.join("config.yaml"), &p),
        Target::Fleet(n) => write_fleet_limits(home, &n, &p),
        Target::Agent(n) => write_agent_limits(&n, &p),
    }
    .map_err(|e| format!("{e:#}"))?;
    Ok(resolve_in(home, scope, name))
}

#[tauri::command] pub fn limits_resolve(scope: String, name: Option<String>) -> Result<LimitsView, String> { Ok(resolve_in(&crate::mur_home_path(), &scope, name.as_deref())) }
#[tauri::command] pub fn limits_set(scope: String, name: Option<String>, patch: LimitsPatchArg) -> Result<LimitsView, String> { set_in(&crate::mur_home_path(), &scope, name.as_deref(), patch) }
#[tauri::command] pub fn limits_remove_stale(scope: String, name: Option<String>, key: String) -> Result<LimitsView, String> {
    let home = crate::mur_home_path();
    remove_stale_key(&home, &target_of(&scope, name.as_deref())?, &key).map_err(|e| format!("{e:#}"))?;
    Ok(resolve_in(&home, &scope, name.as_deref()))
}
```

  (`write_agent_limits` prints a line to stdout — harmless in the Hub; if it bothers, add a `quiet` variant in mur-core. The `upsert_global_limits` println likewise.)

  `lib.rs`: `mod limits;` and `limits::limits_resolve, limits::limits_set, limits::limits_remove_stale,` in `generate_handler!`.

  `fleet.rs`: `FleetLoopView` drops `max_iterations`, `budget_usd`; `FleetDetail` gains `pub limits: limits::LimitsView` (= `limits::resolve_in(&home, "fleet", Some(&name))`); `fleet_set_loop(name, trigger: Option<String>, done_when: Option<String>)` calls `cmd_fleet_set_loop(&home, &name, trigger, None, None, None, done_when)`; `fleet_run_loop(name, app)` passes `None, None, None, None`. Fix the tests in `fleet.rs` that build `FleetLoopView`.

- [x] **2.4 Watch it pass** — `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml --lib` (all), `cargo clippy --manifest-path mur-hub-gui/src-tauri/Cargo.toml --lib -- -D warnings`.

- [x] **2.5 Commit**:

```
feat(hub): limits_resolve / limits_set / limits_remove_stale, and fleet_detail carries its limits

Three commands over the same resolver mur limits uses; the view carries
the §5 badge word. fleet_set_loop keeps trigger and done-when and drops
the guard fields; fleet_run_loop drops them too.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 3 — `LimitsPanel`, mounted at three scopes

**Interfaces.**
- Consumes: the three commands and `LimitsView` (Task 2).
- Produces:

```ts
// components/fleet/types.ts
export interface LimitsRowView { knob: "deadline" | "stuck" | "cost_usd"; value: string; source: string; note: string | null; applies: boolean; local: boolean; raw: string | null }
export interface StaleView { key: string; file: string; value: string; message: string }
export interface LimitsView { scope: "global" | "fleet" | "agent"; name: string | null; rows: LimitsRowView[]; stale: StaleView[]; billable: boolean; needs_restart: boolean; attended_note: string; bounded: "bounded" | "deadline_only" | "unbounded"; error: string | null }
// components/limits/limitsPanel.ts (pure)
export type RowAction = "override" | "reset" | "none";
export function rowAction(row: LimitsRowView): RowAction            // !applies → none; local → reset; else override
export function rowIsDimmed(row: LimitsRowView): boolean             // applies && !local
export function chipLabel(row: LimitsRowView, scope: LimitsView["scope"], t): string   // "this fleet"/"this agent"/"config.yaml"/"built-in default"/legacy…
export function patchForSave(knob, draft: string): LimitsPatchArg    // deadline/stuck: {knob: draft}; cost_usd: {cost_usd: Number(draft)}
export function patchForReset(knob): LimitsPatchArg                  // {unset: [knob]}
export function badgeOf(v: LimitsView, t): { text: string; tone: "ok" | "amber" | "off" }
// components/limits/LimitsPanel.tsx
export function LimitsPanel(props: { scope: LimitsView["scope"]; name?: string; initial?: LimitsView; focusKnob?: LimitsRowView["knob"]; onChanged?: (v: LimitsView) => void }): JSX.Element
```

### Steps

- [ ] **3.1 Write the failing tests** — `mur-hub-gui/ui/src/components/limits/limitsPanel.test.ts`:

```ts
import { describe, it, expect } from "vitest";
import { rowAction, rowIsDimmed, chipLabel, patchForSave, patchForReset, badgeOf } from "./limitsPanel";
import type { LimitsRowView, LimitsView } from "../fleet/types";

const t = (k: string) => k;   // identity — the labels under test are keys
const row = (p: Partial<LimitsRowView>): LimitsRowView => ({ knob: "deadline", value: "2h", source: "fleet.yaml", note: null, applies: true, local: true, raw: "2h", ...p });
const view = (p: Partial<LimitsView>): LimitsView => ({ scope: "fleet", name: "dev", rows: [], stale: [], billable: false, needs_restart: false, attended_note: "", bounded: "bounded", error: null, ...p });

describe("row presentation (§10.1)", () => {
  it("a local value is solid with Reset; an inherited one is dimmed with Override; a non-applying cost row has no action", () => {
    expect(rowAction(row({ local: true }))).toBe("reset");
    expect(rowIsDimmed(row({ local: true }))).toBe(false);
    expect(rowAction(row({ local: false, raw: null, source: "built-in default" }))).toBe("override");
    expect(rowIsDimmed(row({ local: false, raw: null }))).toBe(true);
    expect(rowAction(row({ knob: "cost_usd", applies: false, source: "", note: "runs on local models" }))).toBe("none");
  });
  it("chips name the scope in the user's words", () => {
    expect(chipLabel(row({ source: "fleet.yaml" }), "fleet", t as never)).toBe("limits.chip.thisFleet");
    expect(chipLabel(row({ source: "profile.yaml" }), "agent", t as never)).toBe("limits.chip.thisAgent");
    expect(chipLabel(row({ source: "~/.mur/config.yaml" }), "fleet", t as never)).toBe("limits.chip.config");
    expect(chipLabel(row({ source: "built-in default" }), "global", t as never)).toBe("limits.chip.builtIn");
    expect(chipLabel(row({ source: "fleet.yaml (legacy loop.deadline)" }), "fleet", t as never)).toBe("limits.chip.legacy");
  });
});

describe("patches", () => {
  it("save writes the one knob; reset unsets it (never writes the parent's value)", () => {
    expect(patchForSave("deadline", " 2h ")).toEqual({ deadline: "2h" });
    expect(patchForSave("stuck", "off")).toEqual({ stuck: "off" });
    expect(patchForSave("cost_usd", "5")).toEqual({ cost_usd: 5 });
    expect(patchForReset("stuck")).toEqual({ unset: ["stuck"] });
  });
});

describe("badge (§10.2)", () => {
  it("bounded / deadline only (amber) / unbounded", () => {
    expect(badgeOf(view({ bounded: "bounded" }), t as never)).toEqual({ text: "limits.badge.bounded", tone: "ok" });
    expect(badgeOf(view({ bounded: "deadline_only" }), t as never)).toEqual({ text: "limits.badge.deadlineOnly", tone: "amber" });
    expect(badgeOf(view({ bounded: "unbounded" }), t as never)).toEqual({ text: "limits.badge.unbounded", tone: "off" });
  });
});
```

- [ ] **3.2 Watch it fail** — `cd mur-hub-gui/ui && npx vitest run limitsPanel`.

- [ ] **3.3 Implement the helper** — `limitsPanel.ts` exactly per the Interfaces; `chipLabel`: `source.includes("legacy")` → `limits.chip.legacy`; `"built-in default"` → `builtIn`; `"~/.mur/config.yaml"` → `config`; `"fleet.yaml"` → scope === "fleet" ? `thisFleet` : `fleet`; `"profile.yaml"` → scope === "agent" ? `thisAgent` : `agent`; `"command-line flag"` → `flag`.

- [ ] **3.4 The component** — `LimitsPanel.tsx`:

```tsx
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../i18n";
import type { LimitsRowView, LimitsView } from "../fleet/types";
import { badgeOf, chipLabel, patchForReset, patchForSave, rowAction, rowIsDimmed } from "./limitsPanel";

export interface LimitsPanelProps {
  scope: LimitsView["scope"];
  name?: string;
  initial?: LimitsView;          // fleet detail already carries one; global/agent fetch
  focusKnob?: LimitsRowView["knob"];
  onChanged?: (v: LimitsView) => void;
}

/** One panel, three scopes (spec §10.1). Every value and every "applies" comes
 *  from limits_resolve; this component only decides which affordance to show. */
export function LimitsPanel({ scope, name, initial, focusKnob, onChanged }: LimitsPanelProps) {
  const { t } = useT();
  const [view, setView] = useState<LimitsView | null>(initial ?? null);
  const [editing, setEditing] = useState<LimitsRowView["knob"] | null>(focusKnob ?? null);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    if (initial) { setView(initial); return; }
    invoke<LimitsView>("limits_resolve", { scope, name: name ?? null }).then(setView).catch((e) => setErr(String(e)));
  }, [scope, name, initial]);

  const apply = async (cmd: "limits_set" | "limits_remove_stale", args: Record<string, unknown>) => {
    setBusy(true); setErr(null);
    try {
      const v = await invoke<LimitsView>(cmd, { scope, name: name ?? null, ...args });
      setView(v); setEditing(null); onChanged?.(v);
    } catch (e) { setErr(String(e)); }      // the CLI parser's words, e.g. "limits.deadline: `soon` is not a duration"
    finally { setBusy(false); }
  };

  if (!view) return <p className="overview-now__sub">{err ?? "…"}</p>;
  return (
    <div className="limits-panel">
      {view.error && <div className="fleet-settings__warning">{t("limits.error")}: {view.error}</div>}
      {view.stale.map((s) => (
        <div key={s.key} className="limits-stale">
          <span>{t("limits.stale", { key: s.key, value: s.value, file: s.file })}</span>
          <button type="button" className="btn btn--link" disabled={busy} onClick={() => apply("limits_remove_stale", { key: s.key })}>{t("limits.remove")}</button>
        </div>
      ))}
      {view.rows.map((row) => {
        const action = rowAction(row);
        const isEditing = editing === row.knob;
        return (
          <div key={row.knob} className={`limits-row${rowIsDimmed(row) ? " limits-row--inherited" : ""}`}>
            <span className="limits-row__knob mono">{row.knob}</span>
            {action === "none" ? (
              <span className="limits-row__note">{row.note}</span>
            ) : isEditing ? (
              <>
                <input autoFocus value={draft} onChange={(e) => setDraft(e.target.value)} placeholder={row.knob === "cost_usd" ? "5" : row.knob === "stuck" ? "10m · off" : "2h"} />
                <button type="button" className="btn btn--primary" disabled={busy || !draft.trim()} onClick={() => apply("limits_set", { patch: patchForSave(row.knob, draft) })}>{t("limits.save")}</button>
                <button type="button" className="btn btn--link" onClick={() => setEditing(null)}>{t("limits.cancel")}</button>
              </>
            ) : (
              <>
                <b className="limits-row__value">{row.value}</b>
                <span className="limits-chip">{chipLabel(row, scope, t)}</span>
                {action === "reset" ? (
                  <button type="button" className="btn btn--link" disabled={busy} onClick={() => apply("limits_set", { patch: patchForReset(row.knob) })}>{t("limits.reset")}</button>
                ) : (
                  <button type="button" className="btn btn--link" disabled={busy} onClick={() => { setDraft(row.raw ?? ""); setEditing(row.knob); }}>{t("limits.override")}</button>
                )}
                {row.note && <span className="limits-row__note">{row.note}</span>}
              </>
            )}
          </div>
        );
      })}
      {err && <div className="fleet-settings__warning">{err}</div>}
      <p className="fleet-settings__hint">{view.attended_note}{view.needs_restart ? ` · ${t("limits.restartRequired")}` : ""}</p>
    </div>
  );
}
```

  i18n (`en.ts`, and `zh-TW.ts` with the Traditional Chinese wording):

```ts
  "limits.title": "Execution limits",
  "limits.chip.thisFleet": "this fleet", "limits.chip.thisAgent": "this agent", "limits.chip.fleet": "fleet.yaml", "limits.chip.agent": "profile.yaml",
  "limits.chip.config": "config.yaml", "limits.chip.builtIn": "built-in default", "limits.chip.legacy": "legacy loop.* key", "limits.chip.flag": "command-line flag",
  "limits.override": "Override here", "limits.reset": "Reset to inherited", "limits.save": "Save", "limits.cancel": "Cancel",
  "limits.stale": "{key}: {value} in {file} is ignored", "limits.remove": "Remove",
  "limits.restartRequired": "restart the agent to apply (Stop, then Start)",
  "limits.error": "limits could not be resolved",
  "limits.badge.bounded": "bounded ✓", "limits.badge.deadlineOnly": "bounded by deadline only · no cost cap", "limits.badge.unbounded": "unbounded — will not auto-run",
  "limits.card.deadline": "Deadline", "limits.card.stuck": "Stuck", "limits.card.costCap": "Cost cap",
  "limits.stopped": "stopped: {reason}", "limits.finished": "finished", "limits.openSettings": "Adjust",
```

  (`t(key, vars)` interpolation: confirm the `useT` signature supports `{key}` placeholders — `fleet.rowSubtitle` uses `{ count }`, so it does.)

  CSS (`styles/` — the file that holds `.fleet-settings__row`): `.limits-row { display:grid; grid-template-columns: 7rem 6rem auto 1fr; gap: .5rem; align-items:center; padding:.25rem 0 } .limits-row--inherited .limits-row__value, .limits-row--inherited .limits-chip { opacity:.55 } .limits-chip { font-size:.75rem; padding:.05rem .4rem; border-radius:999px; background: var(--surface-2, #eee) } .limits-stale { display:flex; gap:.5rem; color: var(--amber, #b7791f); padding:.25rem 0 } .fleet-detail__bounded--ok { color: var(--green, #2f855a) } .fleet-detail__bounded--amber { color: var(--amber, #b7791f) } .fleet-detail__bounded--off { color: var(--red, #c53030) }` — use the theme variables the stylesheet already defines (grep `--amber`/`--green`; if absent, use the closest existing status colours the `fleet-job__status--failed` rule uses).

- [ ] **3.5 Mount at three scopes** —
  - `GeneralSettings.tsx`: after the fleet-autorun row, a `settings-row` with `<h4>{t("limits.title")}</h4><LimitsPanel scope="global" />`.
  - `FleetSettings.tsx`: delete the `maxIter`, `deadline`, `budget` state and their three `fleet-settings__row` blocks + `budgetWarning`; `handleSaveSettings` sends `{ name, trigger, doneWhen }` only; `settingsAreValid(trigKind, trigValue)` (drop the `deadline` parameter in `fleetSettingsForm.ts` and its tests; `loopDeadlineIsValid` stays if `FleetHeader` still uses it for the legacy string, else delete it). Insert `<h4>{t("limits.title")}</h4><LimitsPanel scope="fleet" name={detail.name} initial={detail.limits} focusKnob={focusKnob} onChanged={onLimitsChanged} />` between the done-when block and Save; `FleetSettings` receives `focusKnob?: "deadline"|"stuck"|"cost_usd"` and `onLimitsChanged` from `FleetHost` (Task 5 sets the focus).
  - `OverviewTab.tsx` (agent): a new `detail-card` after the "glance" card: eyebrow `t("limits.title")`, `<LimitsPanel scope="agent" name={agentName} />`. No fifth tab.

- [ ] **3.6 Watch it pass** — `npx vitest run` (all), `npx tsc --noEmit`. Fix every reference to the removed `FleetLoopView.max_iterations`/`budget_usd` the compiler names (`fleetSettingsForm.test.ts` fixtures, `FleetOverview.tsx` — Task 4 rewrites those cards; for now make it compile by removing the two cards, Task 4 puts the new ones in).

- [ ] **3.7 Commit**:

```
feat(hub): LimitsPanel at three scopes — the Hub renders what the CLI resolves

One component, driven by limits_resolve / limits_set / limits_remove_stale.
Inherited rows are dimmed with a source chip and Override; local rows are
solid with Reset (which deletes the key); a non-applying cost row is one
line of text, never a disabled input; stale keys are amber rows with
Remove. Mounted on Settings → General, the fleet Settings tab (replacing
the guard inputs) and the agent Overview.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 4 — the fleet Overview cards and the bounded badge

**Interfaces.**
- Consumes: `FleetDetail.limits: LimitsView`, `badgeOf` (Task 3).
- Produces: `FleetOverview` cards `Last auto-run · Deadline · Stuck | Cost cap · Done when`; `fleetMeta(detail, t)` in `FleetHeader.tsx` appends `<span className={`fleet-detail__bounded fleet-detail__bounded--${tone}`}>{text}</span>`, and for `unbounded` wraps it in a button that calls `onGoTo("settings")` (add `onGoTo` to `fleetMeta`'s parameters; grep its callers).

### Steps

- [ ] **4.1 Write the failing test** — `fleetSettingsForm.test.ts` (or a new `fleetOverview.test.ts` beside `FleetOverview.tsx` if the pure helper lives there):

```ts
import { statCards } from "../detail/fleet/fleetOverviewCards";
it("cards are deadline / stuck / done-when, and cost cap replaces stuck on a capped billable fleet", () => {
  const rows = (cost: string) => [
    { knob: "deadline", value: "2h", source: "fleet.yaml", note: null, applies: true, local: true, raw: "2h" },
    { knob: "stuck", value: "10m", source: "built-in default", note: null, applies: true, local: false, raw: null },
    { knob: "cost_usd", value: cost, source: cost === "—" ? "built-in default" : "fleet.yaml", note: null, applies: true, local: cost !== "—", raw: cost === "—" ? null : "5" },
  ] as const;
  expect(statCards({ ...view, billable: false, rows: rows("—") as never }, t as never).map((c) => c.label)).toEqual(["fleet.settings.lastRun", "limits.card.deadline", "limits.card.stuck", "fleet.settings.doneWhen"]);
  expect(statCards({ ...view, billable: true, rows: rows("$5.00") as never }, t as never)[2]).toEqual({ value: "$5.00", label: "limits.card.costCap" });
});
```

- [ ] **4.2 Implement** — `components/detail/fleet/fleetOverviewCards.ts`: `statCards(limits: LimitsView, loop: FleetLoopView | null, t): {value, label}[]` — first card `lastRunLabel(loop?.last_run)`, second the deadline row's value, third `cost_usd` row's value + `limits.card.costCap` when `limits.billable && costRow.local`, else the stuck row's value + `limits.card.stuck`, fourth done-when as today. `FleetOverview.tsx` renders `statCards(detail.limits, loop, t)`. `FleetHeader.tsx` `fleetMeta` appends the badge from `badgeOf(detail.limits, t)`.

- [ ] **4.3 Watch it pass** — `npx vitest run`, `npx tsc --noEmit`. Commit:

```
feat(hub): fleet Overview cards read the resolved limits; header shows the §5 badge

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 5 — stop reasons where the Hub user is looking

**Interfaces.**
- Consumes: the fleet channel's `state-change` events (`payload.stop_reason`, `payload.remedy`, `payload.run_id`, written by `loop_run::emit_stop_event`), `JobRow.run_id`, `mur_channel::ChannelService`.
- Produces: Tauri `JobRow { …, stop_reason: Option<String>, remedy: Option<String> }`, `FleetDetail.last_stop: Option<StopInfo { stop_reason, remedy, at, run_id }>`; `pub(crate) fn stop_of(events: &[ChannelEvent], run_id: &str) -> Option<StopInfo>` (matches `payload.run_id == run_id` or `run_id` starting with `{payload.run_id}-` — the loop's per-iteration runs are named under the parent, step 5); UI: `FleetJobs` status cell shows `t("limits.stopped", {reason})` when `stop_reason` is set and not `converged`, `t("limits.finished")` when `converged`; `FleetOverview` shows a `■ stopped: <reason>` line under the cards with `t("limits.openSettings")` → `onGoTo("settings", knob)`; `FleetHost` keeps `settingsFocus` state and passes `focusKnob` to `FleetSettings`. The knob for a reason: `deadline → deadline`, `stuck → stuck`, `budget → cost_usd`, else none.

### Steps

- [ ] **5.1 Write the failing tests** — Tauri `fleet.rs` tests:

```rust
    #[test]
    fn stop_of_finds_the_runs_state_change_and_its_children() {
        use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
        let ev = |run_id: &str, reason: &str| ChannelEvent {
            seq: 1, ts: chrono::Utc::now(), actor: ChannelActor::System, kind: EventKind::StateChange,
            payload: serde_json::json!({"from":"working","to":"failed","stop_reason": reason,"remedy":"raise it","run_id": run_id}),
            idempotency_key: None, sig: None, key_version: None,
        };
        let events = vec![ev("fleet-dev-abc", "deadline")];
        assert_eq!(stop_of(&events, "fleet-dev-abc").unwrap().stop_reason, "deadline");
        assert_eq!(stop_of(&events, "fleet-dev-abc-3").unwrap().stop_reason, "deadline", "an iteration run named under the loop");
        assert!(stop_of(&events, "run-other").is_none());
    }
```

  (`ChannelEvent` may have more fields — copy the literal from a test in `work.rs`.) UI `fleetJobs.test.ts`: `statusLabel(job, t)` → `"limits.stopped"` for `stop_reason: "deadline"`, `"limits.finished"` for `"converged"`, `"fleet.status.done"` when absent; `knobFor("budget") === "cost_usd"`.

- [ ] **5.2 Implement** — `fleet.rs`: `stop_of` scans events newest-first for `kind == StateChange` with a `stop_reason`; `fleet_jobs` loads the fleet's channel events once and fills `stop_reason`/`remedy` for rows with a `run_id`; `fleet_detail` sets `last_stop` from the newest such event. UI: `fleetJobs.ts` pure helpers `statusLabel`, `knobFor`; `FleetJobs.tsx` uses `statusLabel`; `FleetOverview.tsx` renders `last_stop` with the remedy text and the `Adjust` button; `FleetHost.tsx` state `settingsFocus` → `FleetSettings focusKnob`.

- [ ] **5.3 Watch it pass** — Tauri `cargo test --lib`, `npx vitest run`, `npx tsc --noEmit`; Hub `cargo check` + clippy last. Commit:

```
feat(hub): job rows and the fleet Overview show the channel's stop reason, with Adjust

■ stopped: deadline 1h opens the Settings tab with that knob focused;
finished is reserved for converged, same word rule as the CLI.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## After the last task

- One PR: `feat(hub): LimitsPanel at three scopes, §5 badge, stop reasons (execution-limits Hub 3b)`. It touches mur-core (Task 1) and the workspace-excluded Hub; CI's Hub job needs `ui/dist` — it builds it.
- Real-machine: build the Hub `.app` per `gotcha_hub_local_app_build_recipe` (a `cargo tauri dev` cannot see computer-use, and `tauri build` ships a stale `ui/dist` unless `npm run build` ran first — both are known traps). Then: Settings → General shows three rows with chips; `develop-rust` → Settings shows `deadline 8h · this fleet [Reset]`, `stuck 10m · built-in [Override]`, the cost row as text; header badge `bounded ✓`; an agent Overview shows the Limits card and, for one with a stale `hitl.*` key, an amber Remove row; a fleet whose last loop stopped on deadline shows `■ stopped: deadline` with Adjust landing on the deadline row.
- `update-docs`: Hub section of the docs site (`hub.md` or the product page's Hub card) gets one paragraph on the panel.
