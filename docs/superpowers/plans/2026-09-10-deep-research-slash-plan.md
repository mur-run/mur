# `/deep-research` Slash Command + Agent-Visible Progress — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `mur-executing-plans` (or `mur-delegate-dev` when a running coder agent is available) to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking — tick them in this file, not in memory.

**Goal:** A built-in murmur `/deep-research` slash command, a `mur-deep-research` skill that documents the real flag surface, and a `fleet_run` tool that returns a `run_id` an agent can poll with `mur_job_status` — so a deep-research run is never a blind wait.

**Architecture:** One shared reader (`progress::load_view`) feeds three surfaces: the CLI panel (already there), the murmur slash card, and `mur_job_status`. The loop learns its `run_id` from `MUR_RUN_ID` and registers itself in `run_status`; `fleet_run` mints that id and hands it back. No new event channel — the progress file is the contract.

**Tech Stack:** Rust (edition 2024), tokio, serde_json, uuid v7, existing `run_status` store, existing atomic progress writer.

**Spec:** `docs/superpowers/specs/2026-09-10-deep-research-slash-design.md`

## Global Constraints

- Every progress / run-record write stays best-effort (`tracing::debug!` on error, continue). A bookkeeping failure must never fail, slow, or change the loop.
- Existing loop stdout lines stay byte-identical; additions only.
- `render_progress`'s three existing fixtures (`panel.rs:213-258`) must produce identical output after the `&ProgressView` refactor.
- `fleet_run` default behaviour (`wait` omitted) stays byte-for-byte except for the leading `run_id:` line.
- The slash command runs with the **human's** privileges (a `mur` subprocess, like `!cmd`) — it never calls the agent's `fleet_run` tool.
- Build env: PATH `~/.rustup/toolchains/stable-aarch64-apple-darwin/bin`, `ORT_STRATEGY=download`, `MUR_WEB_DIST=$HOME/Projects/mur-web/dist`. Test via `cargo nextest run -p <crate> <filter>`; `cargo fmt` + `cargo clippy -p <crate> -- -D warnings` before every commit. **One** build at a time — never fan out cargo.

## Decision made at plan time: who registers the run

`fleet_run` lives in `mur-agent-runtime/src/tools/fleet_run.rs`, and `mur-agent-runtime` depends on `mur-common` only — **no `mur-core`**, so it cannot call `run_status::store::save`. Rather than moving `RunState` into `mur-common`, the **child loop registers itself** using the `MUR_RUN_ID` it was handed (Task 2). `fleet_run` only mints the id, passes it in the env, and reports it (Task 3). Consequence: for a few hundred ms after spawn, `mur_job_status <id>` may answer "no run recorded"; the `wait:false` output says so explicitly. The `runs/` sandbox carve-in (Task 3, Step 1) is what lets the child write the record.

## Outcome vocabulary (corrected from the spec's 7 to the real 9)

`loop_run.rs:334` `outcome_label`:

| `outcome` | `run_status::State` |
|---|---|
| `converged`, `max-iterations`, `deadline`, `budget`, `queue-drained` | `Done` |
| `stopped`, `commander-killed` | `Stopped` |
| `awaiting-approval` | `Blocked` |
| `stuck` | `Failed` |

---

### Task 1: D0 — `progress::load_view` + panel refactor

**Files:**
- Modify: `mur-core/src/cmd/fleet/progress.rs` (`load` at `:158` returns `Option<(RunProgress, SystemTime)>` — note `SystemTime`, not `u64` as the spec says)
- Modify: `mur-core/src/cmd/deep_research/panel.rs` (`render_progress(p: &RunProgress, mtime_age_secs: u64)` at `:46`; fixtures `:213-258`)

**Interfaces:**
- Consumes: `RunProgress`, `STALE_AFTER_SECS`, `load` (existing).
- Produces (used by Tasks 3, 4):
  - `pub struct ProgressView { pub progress: RunProgress, pub age_secs: u64, pub stale: bool, pub live: bool }`
  - `pub fn view_from(progress: RunProgress, age_secs: u64) -> ProgressView` — pure; `stale = finished_at.is_none() && age_secs > STALE_AFTER_SECS`, `live = finished_at.is_none() && !stale`.
  - `pub fn load_view(mur_home: &Path, fleet: &str) -> Option<ProgressView>` — `load()` + mtime→`age_secs` (saturating, `0` if the clock is behind) + `view_from`.
  - `panel::render_progress(v: &ProgressView) -> String` (signature change; body unchanged apart from reading `v.progress` / `v.age_secs`).

- [ ] **Step 1: Write the failing tests** (in `progress.rs` `mod tests`)

```rust
#[test]
fn view_truth_table() {
    let mut p = sample(); // existing fixture helper
    p.finished_at = None;
    let v = view_from(p.clone(), 5);
    assert!(v.live && !v.stale);
    let v = view_from(p.clone(), STALE_AFTER_SECS + 1);
    assert!(!v.live && v.stale);
    p.finished_at = Some("2026-09-10T00:00:00Z".into());
    let v = view_from(p, STALE_AFTER_SECS + 1);
    assert!(!v.live && !v.stale, "finished runs are neither live nor stale");
}

#[test]
fn load_view_missing_file_is_none() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(load_view(tmp.path(), "nope").is_none());
}

#[test]
fn load_view_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    sample().save(tmp.path(), "f");
    let v = load_view(tmp.path(), "f").expect("view");
    assert_eq!(v.progress.run_id, "r1");
    assert!(v.age_secs < 5);
    assert!(v.live);
}
```

- [ ] **Step 2: Run** `cargo nextest run -p mur-core progress::tests` — expect compile failure (`view_from` missing).
- [ ] **Step 3: Implement** `ProgressView`, `view_from`, `load_view` in `progress.rs`. Keep `load` public and untouched.
- [ ] **Step 4: Refactor** `panel.rs::render_progress` to `&ProgressView`; update its sole caller in `panel.rs` to go through `load_view`. Update the three fixtures to build a `ProgressView` via `view_from(p, age)` with the same `age` values they passed before — **assertions unchanged**.
- [ ] **Step 5: Run** `cargo nextest run -p mur-core progress panel` — all green, including the three untouched panel assertions.
- [ ] **Step 6:** `cargo fmt && cargo clippy -p mur-core -- -D warnings`; commit `feat(progress): load_view shared reader; panel renders ProgressView`.

---

### Task 2: D3(a) — `MUR_RUN_ID` + loop self-registration in `run_status`

**Files:**
- Modify: `mur-core/src/cmd/fleet/loop_run.rs` (id minted `:420`; per-iteration save block `:673-680`; terminal write `:719-725`; `outcome_label` `:334`)
- Reference only: `mur-core/src/executor/dag.rs:1055-1067` (canonical `RunState` construction), `run_status/store.rs:25` `save`, `:92` `update`, `run_status/heartbeat.rs:27` `beat_once`

**Interfaces:**
- Consumes: `run_status::{RunState, RunKind::Fleet, State, RUN_SCHEMA}`, `store::{save, update}`, `heartbeat::beat_once`.
- Produces:
  - `pub const RUN_ID_ENV: &str = "MUR_RUN_ID";` (in `loop_run.rs`, `pub` so Task 3's tool and Task 4 can name it without a string literal — tool crate re-declares it since it lacks `mur-core`; keep the two in sync via the test in Task 3 Step 5).
  - `fn resolve_run_id_from(env: Option<&str>) -> (String, bool)` — `(value, true)` when `Some` and non-empty, else `(uuid v7, false)`. Pure; the caller passes `std::env::var(RUN_ID_ENV).ok().as_deref()`. (`temp_env` is not a dep — don't add one for this.)
  - `fn state_for_outcome(stop: LoopStop) -> State` — the table above.
  - Env contract: when `MUR_RUN_ID` is set, the loop (i) uses it as `RunProgress.run_id`, (ii) writes a `RunKind::Fleet` `RunState` (label = fleet name, pid = self, `state: Running`, heartbeat = now) via `store::save` before the first iteration, (iii) calls `heartbeat::beat_once` in the per-iteration save block, (iv) on exit `store::update`s `state` to `state_for_outcome(stop)`. When unset: behaviour identical to today (no run record).

- [ ] **Step 1: Write the failing tests** (in `loop_run.rs` `mod tests`)

```rust
#[test]
fn resolve_run_id_prefers_env() {
    assert_eq!(resolve_run_id_from(Some("0192f-test")), ("0192f-test".to_string(), true));
    let (id, from_env) = resolve_run_id_from(None);
    assert!(!from_env);
    assert!(uuid::Uuid::parse_str(&id).is_ok());
    let (_, from_env) = resolve_run_id_from(Some(""));
    assert!(!from_env, "empty env value is treated as unset");
}

#[test]
fn outcome_state_mapping_covers_every_variant() {
    use crate::run_status::State::*;
    assert_eq!(state_for_outcome(LoopStop::Converged), Done);
    assert_eq!(state_for_outcome(LoopStop::MaxIterations), Done);
    assert_eq!(state_for_outcome(LoopStop::Deadline), Done);
    assert_eq!(state_for_outcome(LoopStop::Budget), Done);
    assert_eq!(state_for_outcome(LoopStop::QueueDrained), Done);
    assert_eq!(state_for_outcome(LoopStop::Stopped), Stopped);
    assert_eq!(state_for_outcome(LoopStop::CommanderKilled), Stopped);
    assert_eq!(state_for_outcome(LoopStop::AwaitingApproval), Blocked);
    assert_eq!(state_for_outcome(LoopStop::Stuck), Failed);
}
```

- [ ] **Step 2: Run** `cargo nextest run -p mur-core loop_run::tests` — expect compile failure.
- [ ] **Step 3: Implement** `resolve_run_id_from`, `state_for_outcome`; replace the literal `uuid::Uuid::now_v7()` at `:420` with `resolve_run_id_from(std::env::var(RUN_ID_ENV).ok().as_deref())`. Keep `from_env` in a local.
- [ ] **Step 4: Register.** Immediately after the `RunProgress` is built, `if from_env { save RunState }` — copy the construction at `dag.rs:1055-1067`, `kind: RunKind::Fleet`, `label: name.to_string()`, `channel_id: Some(fleet.channel_id.clone())`. Wrap in the same "bookkeeping must never take down real work" `if let Err(e) … debug!` pattern.
- [ ] **Step 5: Heartbeat + terminal.** In the per-iteration block (`:673-680`) add `if from_env { let _ = heartbeat::beat_once(mur_home, &run_id, Utc::now()); }`. In the terminal block (`:719-725`) add `if from_env { let _ = store::update(mur_home, &run_id, |r| r.state = state_for_outcome(stop)); }`.
- [ ] **Step 6: Integration test** — find the existing loop test that drives `run_loop_inner` with a stub fleet (search `fn .*loop.*converge` in `loop_run.rs` tests). Clone it, set `MUR_RUN_ID` (or pass through the factored helper), run to `Converged`, then assert `store::load(home, id)?.unwrap().state == State::Done` and that `.run_progress.json`'s `run_id` equals the env value.
- [ ] **Step 7: Run** `cargo nextest run -p mur-core loop_run run_status` — green.
- [ ] **Step 8:** fmt + clippy; commit `feat(fleet): loop honours MUR_RUN_ID and self-registers in run_status`.

---

### Task 3: D3(b)(c) — `fleet_run` returns `run_id`, `wait:false`, richer `mur_job_status`

**Files:**
- Modify: `mur-agent-runtime/src/sandbox/policy.rs` (`:358` carve-in list `["fleets", "commander", "conversations", "artifacts"]`; guard test `:1150-1178`, list at `:1166`)
- Modify: `mur-agent-runtime/src/tools/fleet_run.rs` (schema `~:80-100`, `execute` `:104`, spawn `:170-186`, timeout error `:188-201`)
- Modify: `mur-mcp-server/src/tools.rs` (`mur_job_status` `:808-843`; tests `:1002-1052`, helper `call_tool_in` `:981`)

**Interfaces:**
- Consumes: Task 1 `load_view`; Task 2 env contract.
- Produces:
  - `fleet_run` input gains `"wait": { "type": "boolean" }` (default `true`).
  - `pub(crate) const RUN_ID_ENV: &str = "MUR_RUN_ID";` in `fleet_run.rs`.
  - `fn run_id_header(run_id: &str, fleet: &str, mur_home: &Path) -> String` — the two-line block from the spec (`run_id: … fleet: … progress: …` / `poll: … stop: …`).
  - Output contract: `wait:true` → `run_id_header` + `"\n"` + today's tail; timeout error text becomes ``fleet run `{run_id}` timed out after {n}s and was killed; … check `mur_job_status```. `wait:false` → `run_id_header` + one line `note: the record appears once the loop starts (~1s); "no run recorded" before that is not a failure.` Refuses with `Execution("a deep-research run is already live: run_id …")` when `.run_progress.json` for the fleet is live (see Step 3 for how the runtime crate reads it without `mur-core`).
  - `mur_job_status` output: existing four lines, then — only when `run.kind == RunKind::Fleet` and `load_view(mur_home, &run.label)` returns a view whose `progress.run_id == run_id` — `"\nprogress: {iteration_summary_line}"` and one `"  running: {desc}"` line per `StepState::Running` step.

- [ ] **Step 1: Sandbox carve-in.** Add `"runs"` to the list at `policy.rs:358` and to the guard test's list at `:1166`. Run `cargo nextest run -p mur-agent-runtime policy` — green. Commit `feat(sandbox): fleet_run carve-in covers ~/.mur/runs`.

- [ ] **Step 2: Write failing tests** in `fleet_run.rs` `mod tests` (`:209`; check whether it already drives a fake `MUR_BIN` — if it only tests `allowed()`/`resolve_timeout_secs`, add a fake `mur` shell script in a tempdir and point `MUR_BIN` at it, and factor `fn build_command(&self, args, run_id) -> tokio::process::Command` so the env assertion needs no spawn):

```rust
#[tokio::test]
async fn wait_false_returns_header_immediately() {
    // harness: fake `mur` that sleeps 5s
    let out = tool.execute(json!({"fleet":"deep-research","goal":"q","wait":false})).await.unwrap();
    let text = out.to_string();
    assert!(text.starts_with("run_id: "));
    assert!(text.contains("poll: mur_job_status"));
    // must have returned well before the child exits
}

#[tokio::test]
async fn wait_true_output_is_prefixed_with_run_id() {
    let out = tool.execute(json!({"fleet":"deep-research","goal":"q"})).await.unwrap();
    assert!(out.to_string().starts_with("run_id: "));
}

#[tokio::test]
async fn wait_false_refuses_when_live() {
    // write a .run_progress.json with finished_at: null and fresh mtime
    let err = tool.execute(json!({"fleet":"deep-research","goal":"q","wait":false})).await.unwrap_err();
    assert!(err.to_string().contains("already live"));
}

#[test]
fn child_env_carries_run_id() {
    // assert the Command built by the tool sets MUR_RUN_ID (factor `build_command` if needed)
}
```

- [ ] **Step 3: Implement in `fleet_run.rs`.** Mint `uuid::Uuid::now_v7()`; `.env(RUN_ID_ENV, &run_id)` on the child. Liveness check without `mur-core`: read `mur_home/fleets/<fleet>/.run_progress.json` as `serde_json::Value`, `finished_at.is_null()` and mtime age `<= 600` ⇒ live (mirror `STALE_AFTER_SECS`; add a comment pointing at `progress.rs` so the two stay in step). For `wait:false`: `.kill_on_drop(false)`, stdout/stderr → `Stdio::null()`, and on unix `.process_group(0)` (tokio is 1.52 in `Cargo.lock`; `tokio::process::Command::process_group` is available directly, no `CommandExt` detour). Spawn, drop the handle, return the header.
- [ ] **Step 4: Schema.** Add `wait` to the JSON schema in the tool's `input_schema`; description verbatim from the spec.
- [ ] **Step 5: Env-name parity test.** In `mur-core` `loop_run.rs` tests, `assert_eq!(RUN_ID_ENV, "MUR_RUN_ID")`; in `fleet_run.rs` tests the same literal. Two literals, one test each — a drift breaks a test, not a run.
- [ ] **Step 6: Run** `cargo nextest run -p mur-agent-runtime fleet_run` — green.
- [ ] **Step 7: `mur_job_status` failing test** in `mur-mcp-server/src/tools.rs` tests (use `call_tool_in` at `:981`):

```rust
#[tokio::test]
async fn job_status_appends_fleet_progress() {
    let home = tempdir();
    // save a RunState { kind: Fleet, label: "deep-research", run_id: "r1", state: Running, .. }
    // save a RunProgress { run_id: "r1", iteration: 2, steps: [one Running "verify s2"] } under fleets/deep-research
    let out = call_tool_in(&home, "mur_job_status", json!({"run_id":"r1"})).await.unwrap();
    let s = out.as_str().unwrap();
    assert!(s.contains("progress: iteration 2"));
    assert!(s.contains("running: verify s2"));
}

#[tokio::test]
async fn job_status_ignores_progress_with_other_run_id() { /* run_id mismatch → no `progress:` line */ }
```

- [ ] **Step 8: Implement** the append in the `"mur_job_status"` arm after the existing `format!` — build the base string, then `if status.run.kind == RunKind::Fleet { if let Some(v) = mur_core::cmd::fleet::progress::load_view(&mur_home, &status.run.label) { if v.progress.run_id == run_id { … } } }`. Path is already public end to end: `lib.rs:26 pub mod cmd` → `cmd/mod.rs:37 pub mod fleet` → `cmd/fleet/mod.rs:19 pub mod progress`. No re-export needed.
- [ ] **Step 9: Run** `cargo nextest run -p mur-mcp-server job_status` — green.
- [ ] **Step 10:** fmt + clippy on all three crates; commit `feat(fleet_run): return run_id, wait:false, job_status shows fleet progress`.

---

### Task 4: D1 — `/deep-research` in murmur

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/app.rs` (`SlashCmd` `:158`; `parse_slash` `:212-271`; `push_system` `:925`; `push_shell` `:1467`)
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (`HELP` `:196`; dispatch `:2222-2271`, `Panel` arm at `:2270`; `!cmd` shell path `:1781`; parity tests `help_name` / `one_of_each` `:3130-3197`)
- Modify: `mur-core/src/cmd/agent/cli/complete.rs` (`COMMANDS` `:164-200`)
- Create: `mur-core/src/cmd/agent/cli/deep_research.rs` (handler, sibling of `panel.rs`)

**Interfaces:**
- Consumes: Task 1 `load_view`, `iteration_summary_line`; `DEFAULT_FLEET_NAME`; `StreamMsg::ShellDone` (the `!cmd` result path).
- Produces:
  - `SlashCmd::DeepResearch(Vec<String>)`; parse words `"deep-research" | "research"`.
  - `pub enum DeepResearchAction { Status, Stop, Setup, Ask(String) }` + `pub fn classify(args: &[String]) -> DeepResearchAction` — `status|stop|setup` only when it is the **sole** word; otherwise `Ask(args.join(" "))`; empty → `Status`.
  - `pub fn handle(app: &mut App, args: &[String])` — dispatch table:
    - `Status` → spawn `mur deep-research` (bare) through the same `!cmd` machinery, result lands as a shell card.
    - `Stop` → spawn `mur fleet stop deep-research`; `push_system("kill-switch written — the loop exits at its next guard check")`.
    - `Setup` → `push_system("run `mur deep-research setup` in a terminal — it asks for egress consent")`. No subprocess.
    - `Ask(q)` → spawn `mur deep-research <q>` (argv, never shell) with `MUR_RUN_ID` set to a fresh uuid v7 (so the human's run is pollable too); start a tokio task that every 5 s reads `load_view(home, DEFAULT_FLEET_NAME)` and, while `live` and the iteration changed, `push_system(iteration_summary_line)`; when the process ends, its output arrives as the shell card via `ShellDone`. Non-blocking: input stays enabled.
  - `COMMANDS` row: `("deep-research", "run the research fleet, or show its status", Args::Fixed(DEEP_RESEARCH_SUBS))` with `const DEEP_RESEARCH_SUBS: &[(&str,&str)] = &[("status","show the fleet panel"),("stop","write the kill-switch"),("setup","how to run the wizard")]`. `research` alias is parser-only (like `theme`/`todo`), not a second row.

- [ ] **Step 1: Write failing tests**

In `app.rs` tests:
```rust
#[test]
fn parse_deep_research() {
    assert!(matches!(parse_slash("/deep-research"), Some(SlashCmd::DeepResearch(v)) if v.is_empty()));
    assert!(matches!(parse_slash("/research what is X"), Some(SlashCmd::DeepResearch(v)) if v == ["what","is","X"]));
}
```
In `deep_research.rs` tests:
```rust
#[test]
fn classify_reserved_words_only_when_sole() {
    use DeepResearchAction::*;
    assert!(matches!(classify(&[]), Status));
    assert!(matches!(classify(&s(&["status"])), Status));
    assert!(matches!(classify(&s(&["stop"])), Stop));
    assert!(matches!(classify(&s(&["setup"])), Setup));
    assert!(matches!(classify(&s(&["status","of","X"])), Ask(q) if q == "status of X"));
}
```
In `mod.rs`: add `SlashCmd::DeepResearch(vec![])` to `one_of_each` and `"deep-research"` to `help_name` — the existing structural test then enforces `HELP` and `COMMANDS` parity.

- [ ] **Step 2: Run** `cargo nextest run -p mur-core cli::app cli::deep_research every_command_is_parsed` — expect compile failures.
- [ ] **Step 3: Parser + enum + HELP + COMMANDS.** Add the variant, the two parse words, a `HELP` line (`/deep-research [question|status|stop|setup]  run the research fleet (/research)`), and the `COMMANDS` row. Run the parity test — green before touching the handler.
- [ ] **Step 4: Handler.** Create `deep_research.rs` with `classify` + `handle`. For the subprocess, extract the spawn half of the `!cmd` path at `mod.rs:1781` into a reusable `fn spawn_shell_card(app, program: &str, args: &[String], env: &[(&str,&str)])` if it is not already argv-shaped — **do not** route through `sh -c` (the question is user text). Wire `SlashCmd::DeepResearch(args) => deep_research::handle(app, &args)` next to the `Panel` arm.
- [ ] **Step 5: Progress ticker.** In `Ask`, spawn the 5 s poller on the app's existing tokio handle; it sends system lines through whatever channel `push_system` is reachable from off-thread (look at how `StreamMsg` is delivered from the streaming task and reuse that variant, or add `StreamMsg::System(String)` if none exists). Stop when `!live` or the process has exited. Dedupe on `iteration` so a slow loop doesn't repeat lines.
- [ ] **Step 6: Manual check** (record in the commit message): `mur agent chat` → `/deep-research` renders the panel card; `/research status of X` starts a run; `/deep-research stop` ends it with `outcome = stopped`; typing while it runs still works.
- [ ] **Step 7: Run** `cargo nextest run -p mur-core cli::` — green. fmt + clippy; commit `feat(murmur): /deep-research slash command with live progress lines`.

---

### Task 5: D2 — `mur-deep-research` skill v0.2.0

**Files:**
- Modify: `mur-core/src/skills/mur_deep_research.yaml` (`version: 0.1.0` → `0.2.0`; replace `content.context`)

**Interfaces:**
- Consumes: everything shipped in Tasks 1–4 (write it last so it describes reality).
- Produces: the skill text. Sections, in order:
  1. The five-invocation table from the spec ("CLI surface").
  2. `provision` and `run` flag tables (verbatim from the spec — they come from `actions.rs:770-836`).
  3. **If you are an agent, not a human** — use `fleet_run {fleet:"deep-research", goal:"<q>"}`; `wait:false` + `mur_job_status <run_id>` for long runs; a `fleet_run denied` error is a config fact (`fleet_run.agents` / `fleet_run.fleets` in `~/.mur/config.yaml`) to report, never to route around; never shell out to `mur deep-research`.
  4. **Reading progress** — `~/.mur/fleets/deep-research/.run_progress.json`, the 9-value `outcome` list, stale after 600 s, `mur_job_status` shows `progress:` for fleet runs.
  5. **From murmur** — `/deep-research …` / `/research …` grammar, one line each.
  6. Kill-switch: `mur fleet stop deep-research`.

- [ ] **Step 1: Failing test.** Find the built-in-skill seed test (`grep -rn "mur_deep_research" mur-core/src --include=*.rs`) and add an assertion that the seeded skill's `version == "0.2.0"` and its context contains `fleet_run` and `MUR_RUN_ID`-free wording (agents never set the env themselves — assert the text does **not** mention `MUR_RUN_ID`).
- [ ] **Step 2:** Edit the YAML. Keep `trigger`/`abstract`/`description` as-is (the abstract still holds).
- [ ] **Step 3: Run** the seed test + `cargo nextest run -p mur-core skills` — green. Confirm the re-seed path replaces a 0.1.0 copy (there is an existing version-bump test for another built-in; mirror it).
- [ ] **Step 4:** commit `docs(skill): mur-deep-research 0.2.0 — flag table, agent path, progress`.

---

## Verification (before claiming done)

- [ ] `cargo nextest run -p mur-core -p mur-agent-runtime -p mur-mcp-server` — all green, one run.
- [ ] `cargo clippy --workspace -- -D warnings` clean.
- [ ] End-to-end from an allowlisted agent: `fleet_run {fleet:"deep-research", goal:"…", wait:false}` → returns `run_id`; `mur_job_status` within 10 s says `running · alive` with a `progress:` line; after `mur fleet stop deep-research`, says `stopped`.
- [ ] Spec test-plan table, row by row, each mapped to a test name in this plan — no row left without one.

## Out of scope (from the spec, restated so nobody "helpfully" adds them)

- Streaming events from loop to agent.
- Concurrent deep-research runs (refused with the live run's id).
- Any change to preflight / egress consent.
