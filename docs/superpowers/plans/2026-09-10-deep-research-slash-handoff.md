# Handoff — `/deep-research` slash command + agent-visible progress (rev 2)

> **For fleet `develop-rust`.** Execute with `mur-executing-plans`, task by task, in the order below. Delegate Rust to `rustsmith`, tests to `qa`, commits to `repomanager`; `pm` ticks the checkboxes **in this file** (not in memory). Stop after each task for review.

**Spec:** `docs/superpowers/specs/2026-09-10-deep-research-slash-design.md`
**Prior plan:** `docs/superpowers/plans/2026-09-10-deep-research-slash-plan.md` — **superseded where it conflicts with this file** (it still describes loop self-registration, a `runs/` carve-in, and a `HELP` constant; none of those exist or apply — see "Decisions"). Treat this handoff as the source of truth; the plan file is kept for its test sketches only.
**Repo:** `/Volumes/Firecuda4tb/Projects/mur`
**Base:** `main` @ `b30221c8` (every anchor below was read at this commit; symbols are the durable anchor if lines drift)
**Branch:** create `feat/deep-research-slash` from `main` before Task 1
**Status:** nothing executed yet — zero of six tasks done

## Problem (why this exists)

Today `mur deep-research "<question>"` is a blind wait: the `fleet_run` tool blocks up to 1800 s (`fleet_run.rs:34`) and returns nothing an agent can poll; `mur_job_status` only knows `run_status` records, and the loop only records **per-iteration** `RunKind::Fleet` runs with ids like `loop-<fleet>-<uuid>-<iter>` (`loop_run.rs:631–632`) that no caller is ever handed. In murmur there is no `/deep-research` at all — the only way to launch a run from the chat is `!mur deep-research …`, whose output is then **sent to the agent as a new turn** (`mod.rs:2584–2598`), which is exactly wrong for a status read. Preflight can also fail for tens of seconds (worker start, re-pin, 10 s worker-up wait per worker — `ask.rs:91–125`) **before** the loop writes any progress file, so a poller sees "nothing" and cannot tell "not started" from "failed".

## Goal in one breath

A murmur `/deep-research` command whose output stays on screen and is never fed back to the agent; a `fleet_run` tool that mints a `MUR_RUN_ID`, can return immediately (`wait:false`), and hands back that id; a `mur_job_status` that answers for that id by falling back to the deep-research progress file; a progress file that records preflight failures and the saved report path; and a skill that documents what actually shipped.

## Decisions already made — do not relitigate

1. **No second whole-loop `RunState`.** `execute_dag` already registers one `RunKind::Fleet` record **per iteration** (`loop_run.rs:631–633` → `dag.rs:1101–1125`). A parallel whole-loop record would make `mur_job_status` and `mur fleet show` disagree. The loop does **not** self-register, does **not** heartbeat (`heartbeat::beat_once` at `run_status/heartbeat.rs:27` exists but has no record to beat against here), and does **not** touch `run_status::store`.
2. **`MUR_RUN_ID` is the join key.** `fleet_run` mints it (uuid v7 — `uuid` with `v7` is already a dep at `mur-agent-runtime/Cargo.toml:52`) and passes it in the child's env; the loop honours it as `RunProgress.run_id` instead of minting at `loop_run.rs:420`. `mur_job_status` first checks ordinary run records via `mur_core::run_status::status_of` (`run_status/mod.rs:213`) and, when that returns `None`, **falls back** to finding a `.run_progress.json` whose `run_id` matches. The env-var name is defined **once** as a `const` in `mur-common` (both crates already depend on it; no hardcoded strings — CLAUDE.md).
3. **The preflight-before-progress window is tens of seconds**, not sub-second. Points C/D/E in `cmd_ask` (`ask.rs:95`, `:99–107`, `:114–125`) start workers, re-pin, and wait up to 10 s each before `run_guarded` writes the first progress file (`loop_run.rs:435`). The `wait:false` output must say so verbatim: `progress appears after preflight (may take tens of seconds)`.
4. **`/deep-research status` and `/deep-research ask` output is user-visible only.** It must **not** become a new agent turn. Do not construct `StreamMsg::ShellDone` and do not go through `route_shell_output` (`mod.rs:1948`). Use `app.push_system(...)` for synchronous arms (as `Open` does at `mod.rs:2406–2416`) and `StreamMsg::Note(String)` on `tx` for async work (`Note → push_system` at `mod.rs:2560`). There is no `pending_shell` stash in the code; do not look for one.
5. **Progress schema grows two optional fields, no version bump.** Add to `RunProgress` (`progress.rs:61–73`): `artifact_path: Option<PathBuf>` and `error: Option<String>`, both `#[serde(default, skip_serializing_if = "Option::is_none")]`. `schema_version` stays `1` (`loop_run.rs:419`). Old files load; new files are readable by old binaries.
6. **Terminal outcome is written by the loop first; `cmd_ask` back-fills `artifact_path`.** Order today: loop stamps `finished_at`/`outcome` at `loop_run.rs:719–727`, then `cmd_ask` extracts + saves the report at `ask.rs:147–153` and only `println!`s the path. New: after `save_report` succeeds, `cmd_ask` **reloads** the progress file (`progress::load`), sets `artifact_path`, saves again. While `finished_at.is_some() && outcome ∈ Done-set && artifact_path.is_none()`, `mur_job_status` renders `report being saved`.
7. **`cmd_ask` wraps its body with the run id.** Rename the current body (`ask.rs:70–156`) to an inner fn; `cmd_ask` resolves the run id (env, else mint), then matches the inner result. Any preflight `Err` (points A–G, `ask.rs:85–133`) writes a **minimal** progress record — `run_id`, `question`, `started_at`, `finished_at = now`, `outcome = Some("failed")`, `error = Some(summary)`, `iteration 0`, `steps []` — or, when a same-`run_id` progress file already exists, updates only `outcome`/`error`/`finished_at`. Loop-internal failures (`ask.rs:142` and below) keep being written by the loop; the wrapper does not overwrite a loop-written outcome. Note: `outcome_label` (`loop_run.rs:334–345`) has exactly **nine** terms and is unchanged; `failed` is the wrapper-only term already reserved in the field doc comment at `progress.rs:66`.
8. **No sandbox carve-in work.** `mur_common::paths::RUN_STATE_DIRS` (`paths.rs:19`) already contains `RUNS = "runs"` and `policy.rs:358` / guard test `:1185` iterate that array. The old "add `runs`, don't add a second loop" step is deleted.
9. **User-facing → docs trio in the definition of done** (`CLAUDE.md:158`): `README.md`, the docs site, and the product page.
10. Kept from rev 1: the outcome vocabulary is **nine** (`converged`, `max-iterations`, `deadline`, `budget`, `queue-drained` → done; `stopped`, `commander-killed` → stopped; `awaiting-approval` → blocked; `stuck` → failed); `progress::load()` returns `(RunProgress, SystemTime)` (`progress.rs:144`) so age-seconds is computed by the caller (`panel.rs:137–138`); tokio is `1.52.3` (`Cargo.lock:10019–10020`) so `tokio::process::Command::process_group` is a direct method; `mur-mcp-server` already depends on `mur-core` (`mur-mcp-server/Cargo.toml:14`); the slash command runs with the **human's** privileges (in-process mur-core calls or an argv `mur` subprocess), never via the agent's `fleet_run` tool; the progress file is the contract — no new event channel.

## Task order and crate ownership

| # | Task | Crates | Reviewer gate |
|---|---|---|---|
| 1 | Progress model: `artifact_path` + `error`, `ProgressView`/`load_view`, outcome→state mapping | mur-core | 3 existing panel fixtures byte-identical; old JSON without the new fields still loads |
| 2 | `RUN_ID_ENV` const in mur-common; loop honours it at `loop_run.rs:420` | mur-common, mur-core | pure `resolve_run_id_from` tests; loop test asserts `run_id == env value` |
| 3 | `cmd_ask` wrapper: preflight-failure record, `artifact_path` back-fill, `run_id:` first line | mur-core | one test per wrapper branch (fresh failed record / update existing / back-fill) |
| 4 | `fleet_run`: mint + pass `MUR_RUN_ID`, `wait:false` detach, `run_id:` header; `mur_job_status` progress fallback | mur-agent-runtime, mur-mcp-server | default `fleet_run` output unchanged except leading `run_id:` line; `job_status` tests for running / report-being-saved / failed / done |
| 5 | `/deep-research` (+ `/research` alias) in murmur, new submodule, Note-only output, 5 s ticker | mur-core | help/complete parity tests pass; no `ShellDone` constructed in the new module |
| 6 | Skill bump to 0.2.0 + docs trio — written last | mur-core, README, external docs | describes what shipped, nothing more |

1 → 2 → 3 → 4 are sequential. Task 5 depends on 1 (and on 3 for the `run_id:` line it may echo). Task 6 depends on everything. **One cargo build at a time — never fan out cargo.**

## Build / test env (non-negotiable)

```
PATH=$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH
ORT_STRATEGY=download
MUR_WEB_DIST=$HOME/Projects/mur-web/dist
```

- Test: `cargo nextest run -p <crate> <filter>`
- Before every commit: `cargo fmt` then `cargo clippy -p <crate> -- -D warnings`
- Commit per task, conventional prefix (`feat(progress):`, `feat(fleet):`, `feat(deep-research):`, `feat(fleet_run):`, `feat(mcp):`, `feat(murmur):`, `docs(skill):`)
- **≤ 800 lines per source file** (CLAUDE.md). `cli/mod.rs` is 3768 lines and `cli/app.rs` 2797 — both already over; **add no new logic to them beyond the one-line variant, parse arm, dispatch arm, help row, and test-list entries.** All `/deep-research` logic goes in a new `mur-core/src/cmd/agent/cli/deep_research.rs`.

---

## Task 1 — Progress model (mur-core)

**Goal.** `RunProgress` can carry a saved-report path and an error; every reader gets age-seconds and a state verdict from one place.

**Files.** `mur-core/src/cmd/fleet/progress.rs` (266 lines), `mur-core/src/cmd/deep_research/panel.rs` (258 lines).

- [ ] **Step 1: failing tests** in `progress.rs` `mod tests`:
  - `old_json_without_new_fields_loads` — serialize a fixture, delete `artifact_path`/`error` keys, deserialize, both `None`.
  - `new_fields_round_trip` and `none_fields_are_omitted_on_save` (`skip_serializing_if`).
  - `view_from_computes_age_and_liveness` — `ProgressView { progress, age_secs, live }` where `live = finished_at.is_none() && age_secs <= STALE_AFTER_SECS` (`progress.rs:14`).
  - `state_for_outcome_covers_all_nine_plus_failed` — table test over the nine `outcome_label` strings plus `"failed"` → `done | stopped | blocked | failed`; unknown string → `failed`. Assert it against the literal strings so a rename in `loop_run.rs:334–345` breaks a test.
  - `report_saving_is_detected` — terminal + done-set outcome + `artifact_path: None` ⇒ `ProgressPhase::ReportSaving` (or equivalent), with `Some(path)` ⇒ `Done`.
- [ ] **Step 2:** `cargo nextest run -p mur-core progress::tests` — expect compile failure.
- [ ] **Step 3: implement** in `progress.rs`: the two fields (decision 5); `pub struct ProgressView`; `pub fn view_from(p: RunProgress, age_secs: u64) -> ProgressView`; `pub fn load_view(mur_home, fleet) -> Option<ProgressView>` built on `load()` (`:144`) — **keep `load` public and untouched**; `pub fn state_for_outcome(outcome: &str) -> &'static str` (or a small enum) as the single mapping used by Task 4's fallback. If `progress.rs` would exceed 800 lines, split `view.rs` beside it.
- [ ] **Step 4: refactor** `panel.rs::render_progress` (`:46`) to take `&ProgressView`; update `cmd_panel` (`:136–144`) to use `load_view`; rebuild the three fixtures at `:182–258` via `view_from(p, age)` with the same `age` values — **assertions unchanged, byte-identical output**.
- [ ] **Step 5:** `cargo nextest run -p mur-core progress panel` — green.
- [ ] **Step 6:** fmt + clippy; commit `feat(progress): artifact_path/error fields, ProgressView + load_view, outcome→state map`.

---

## Task 2 — `RUN_ID_ENV` + loop honours it (mur-common, mur-core)

**Goal.** One `const` names the env var; the loop uses it as the progress `run_id` when present.

**Files.** `mur-common/src/…` (put the const next to other cross-crate env names — `mur_common::identity::SIGNING_HANDOFF_ENV` at `identity.rs:174` is the precedent; a `pub const RUN_ID_ENV: &str = "MUR_RUN_ID";` in `mur_common::fleet` or `mur_common::paths` is fine — **mur-common holds constants, not logic**), `mur-core/src/cmd/fleet/loop_run.rs` (1445 lines).

- [ ] **Step 1: failing tests** in `loop_run.rs` `mod tests`: `resolve_run_id_from(Some("abc")) == "abc"`, `resolve_run_id_from(Some("")) ` mints, `resolve_run_id_from(None)` mints a v7 uuid string. (Pure fn over `Option<&str>` — no `temp_env` dev-dep exists, so no env mutation in tests.) Plus: `env_name_is_stable` → `assert_eq!(mur_common::…::RUN_ID_ENV, "MUR_RUN_ID")`.
- [ ] **Step 2:** run, expect compile failure.
- [ ] **Step 3: implement** `pub fn resolve_run_id_from(env_value: Option<&str>) -> String` in `loop_run.rs` (or `progress.rs`, whichever Task 3 can reuse); replace the literal at `loop_run.rs:420` with `resolve_run_id_from(std::env::var(RUN_ID_ENV).ok().as_deref())`. **Nothing else in the loop changes** — no `RunState`, no heartbeat (decision 1). The per-iteration `run_id: format!("loop-…")` at `:631` stays as is.
- [ ] **Step 4: loop test** — clone the existing `run_guarded(home, "dev", Some(1), None, None)` test at `loop_run.rs:1154` and assert the progress file's `run_id` is what `resolve_run_id_from` returned for the injected value (thread the resolved id through a local, do not read env in the assertion).
- [ ] **Step 5:** `cargo nextest run -p mur-core loop_run` — green. fmt + clippy; commit `feat(fleet): loop honours MUR_RUN_ID as progress run_id`.

---

## Task 3 — `cmd_ask` wrapper (mur-core)

**Goal.** A poller with the run id always finds an answer: a failed-preflight record, a live loop record, or a finished record with the report path.

**Files.** `mur-core/src/cmd/deep_research/ask.rs` (319 lines).

- [ ] **Step 1: failing tests** in `ask.rs` `mod tests` (today's four at `:237–295` are pure `plan_preflight`/`scope_to_members`; add pure helpers so `cmd_ask` itself need not run):
  - `record_preflight_failure_writes_minimal_record` — tempdir, no progress file; call the helper with `(run_id, question, err)`; `load()` gives `outcome == Some("failed")`, `error` starts with the summary, `finished_at.is_some()`, `iteration == 0`, `steps.is_empty()`.
  - `record_preflight_failure_updates_same_id_only` — existing file with same `run_id` and `outcome: None` → outcome/error/finished_at set, other fields preserved; existing file with a **different** `run_id` and a loop-written outcome → untouched (write a fresh record for the new id is acceptable — pick one behaviour and assert it; recommended: overwrite, since the file is "last run" by design at `loop_run.rs:718`).
  - `backfill_artifact_path_reloads_and_saves` — file with terminal outcome, `artifact_path: None` → after helper, `Some(path)`; a file whose `run_id` differs is left alone.
- [ ] **Step 2:** run, expect compile failure.
- [ ] **Step 3: implement.** Rename the body at `ask.rs:70–156` to `async fn ask_inner(mur_home, question, run_id) -> Result<()>`. `cmd_ask` resolves `run_id = resolve_run_id_from(env)`; if it had to mint, set `RUN_ID_ENV` in the same documented `set_var` block as `MUR_HOME` (`ask.rs:71–78`) so the loop (Task 2) sees the same id. Print `run_id: <id>` as the **first** stdout line (the `fleet_run` tool and terminal users both read it). On `Err` from the preflight points A–G (`:85–133`) call the failure helper; on `Err` from the loop (`:142`) do **not** overwrite (loop already stamped outcome at `loop_run.rs:719–727`) — distinguish by checking whether a progress file with this `run_id` and `Some(outcome)` already exists. After `save_report` succeeds (`:150`), call the back-fill helper (decision 6) and keep the `Report: …` println.
- [ ] **Step 4:** `cargo nextest run -p mur-core deep_research` — green. fmt + clippy; commit `feat(deep-research): run_id line, failed-preflight record, artifact_path back-fill`.

---

## Task 4 — `fleet_run` `wait:false` + `mur_job_status` fallback (mur-agent-runtime, mur-mcp-server)

**Goal.** An agent gets a `run_id` back immediately and can poll it.

**Files.** `mur-agent-runtime/src/tools/fleet_run.rs` (413 lines), `mur-mcp-server/src/tools.rs` (1052 lines — **over 800 already; put the fallback renderer in a new `mur-mcp-server/src/job_status.rs` and call it from the arm**).

- [ ] **Step 1: failing tests** in `fleet_run.rs` `mod tests` (`:274+`; current tests only drive `allowed`, deny paths, and `def_schema_requires_fleet` at `:402` — none spawn). Factor `fn build_command(&self, args, run_id, wait) -> tokio::process::Command` so tests assert without spawning:
  - `child_env_carries_run_id` — `build_command(..).as_std().get_envs()` contains `(RUN_ID_ENV, run_id)`.
  - `wait_false_detaches` — `kill_on_drop` cannot be read back; assert instead on the branch's observable contract: stdout/stderr are `Stdio::null()` (inspect via `as_std()`), and the returned header is `run_id: <id>\nstarted in background — progress appears after preflight (may take tens of seconds); poll mur_job_status <id>` (exact wording lives in one `const`).
  - `def_schema_offers_wait` — schema has `wait: boolean`.
  - `wait_true_header_prefixes_run_id` — output starts with `run_id: <id>\n` and the rest is unchanged.
- [ ] **Step 2:** run, expect compile failure.
- [ ] **Step 3: implement.** Mint `uuid::Uuid::now_v7()`; `.env(RUN_ID_ENV, &run_id)` beside the existing `.env("PATH", …)` at `:207`. For `wait:false`: `.kill_on_drop(false)` (the spawn at `:210` currently uses `true` — a dropped handle would kill the run), `Stdio::null()` for stdout/stderr, `#[cfg(unix)] .process_group(0)`, spawn, drop the handle, return the header. `wait:true` (default) keeps the timeout/`wait_with_output` path at `:237–248` and prepends the `run_id:` line. The signing handoff on stdin (`:213–235`) is unchanged in both modes.
- [ ] **Step 4:** `cargo nextest run -p mur-agent-runtime fleet_run` — green. Commit `feat(fleet_run): mint MUR_RUN_ID, wait:false returns run_id`.
- [ ] **Step 5: failing tests** in `mur-mcp-server` (`call_tool_in` helper at `tools.rs:981`; existing tests `:1002`, `:1038`). Each writes a `.run_progress.json` under `<tmp>/fleets/deep-research/` via `mur_core::cmd::fleet::progress::RunProgress::save` and calls `mur_job_status` with its `run_id`:
  - running (no `finished_at`) → contains `state: running` and a `progress:` line from `iteration_summary_line` (`progress.rs:151`) and an age/staleness hint.
  - terminal `converged` + `artifact_path: None` → contains `report being saved`.
  - terminal `converged` + `artifact_path: Some` → `state: done` and the path.
  - `failed` + `error` → `state: failed` and the error text.
  - unknown id with no matching progress → still `no run recorded` (existing test at `:1038` must stay green).
- [ ] **Step 6: implement** in the `"mur_job_status"` arm (`tools.rs:808–843`): when `status_of` returns `None`, scan `<mur_home>/fleets/*/.run_progress.json` via `progress::load_view` (deep-research fleet first, then any other fleet dir) and match `run_id`; render via `state_for_outcome` (Task 1); liveness for a running record is `alive` if `age_secs <= STALE_AFTER_SECS`, else `STALLED`. Do not add `mur_job_status` to compression (it is already in `AUTO_COMPRESS_SKIP` at `:391`).
- [ ] **Step 7:** `cargo nextest run -p mur-mcp-server job_status` — green. fmt + clippy on both crates; commit `feat(mcp): mur_job_status falls back to deep-research progress by run_id`.

---

## Task 5 — `/deep-research` in murmur (mur-core)

**Goal.** From the chat: see status, start a run, stop it — with output that stays on screen.

**Files.** New `mur-core/src/cmd/agent/cli/deep_research.rs`; one-line touches in `cli/app.rs`, `cli/mod.rs`, `cli/complete.rs`.

Subcommands (fixed list, completion-offered): `status` (default when bare), `ask <question…>`, `stop`. Aliases: `/research` parses to the same variant.

- [ ] **Step 1: failing tests**
  - `app.rs` beside `parse_slash_variants` (`:1751`): `/deep-research` → `DeepResearch(vec![])`; `/deep-research ask why is the sky blue` → `DeepResearch(["ask","why",…])`; `/research status` → same variant.
  - `mod.rs` `help_coverage_tests` (`:3291`): add `SlashCmd::DeepResearch(_) => Some("deep-research")` to `help_name` (`:3300` — exhaustive match, **compile fails until added**) and `SlashCmd::DeepResearch(vec![])` to `one_of_each` (`:3335` — **silently unchecked until added**, per the doc comment at `:3328–3334`). `every_command_is_parsed_documented_and_offered` (`:3400`) then requires the help row and the `COMMANDS` entry.
  - `deep_research.rs` tests: `classify(&[])==Status`, `classify(["ask","a","b"])==Ask("a b")`, `classify(["stop"])==Stop`, `classify(["bogus"])==Usage`; `ticker_line_dedupes_on_iteration` — pure fn over `(last_iteration, &ProgressView) -> Option<String>`; `module_never_builds_shell_done` — a test that reads `include_str!("deep_research.rs")` and asserts it does not contain `ShellDone` or `route_shell_output` (cheap guard for decision 4).
- [ ] **Step 2:** run, expect compile failures.
- [ ] **Step 3: parser + enum + help + completion.** `SlashCmd::DeepResearch(Vec<String>)` after `Panel` (`app.rs:186`), following the `Panel(Vec<String>)` shape; parse arm `"deep-research" | "research" => SlashCmd::DeepResearch(words.map(str::to_string).collect())` between `"panel"` (`app.rs:268`) and `other` (`:271`). Help: extend the `more` row in `help_text()` (`mod.rs:213`) with `/deep-research [status|ask <q>|stop] (research fleet)` — there is **no `HELP` constant**; the stale wording at `mod.rs:3370` and `complete.rs:243` may be corrected in passing. Completion: `const DEEP_RESEARCH_SUBS` beside `PANEL_TABS` (`complete.rs:182`) and a `("deep-research", "run/inspect the research fleet", Args::Fixed(DEEP_RESEARCH_SUBS))` entry alphabetically between `clear` (`:210`) and `effort` (`:211`). `offers()` is `#[cfg(test)]` (`:246`) — never call it from production code. Run the parity tests — green before touching the handler.
- [ ] **Step 4: handler.** `mod deep_research;` next to `mod panel;` (`mod.rs:25`). Dispatch arm next to `Panel` (`mod.rs:2405`): `SlashCmd::DeepResearch(args) => deep_research::handle(app, &args, tx)`.
  - `Status`: **in-process** — `app.push_system(render_panel(&collect_status(&app.home, DEFAULT_FLEET_NAME), load_view(..)))`, same renderer the CLI panel uses (`panel.rs:8`), so the numbers match by construction.
  - `Ask(q)`: refuse with a system line if a live run exists (`load_view(..).live`). Otherwise spawn `std::env::current_exe()` with argv `["deep-research", q]` (argv only — the question is user text, never `sh -c`; `MUR_HOME` = `app.home`) via `tokio::process::Command`, stdout piped; push `running deep research: <q>…`; forward the child's `run_id:` and `Report:` lines and its exit status as `StreamMsg::Note` on `tx`. **Never** `ShellDone` (decision 4). Ticker: every 5 s (a `const`) call `load_view`; emit one `Note` per new `iteration` using the dedupe fn; stop when the child exits or `!live`.
  - `Stop`: in-process kill-switch — call the same function `mur fleet stop deep-research` uses (`mur-core/src/cmd/fleet/control.rs`; verify the symbol) and push a system line.
  - `Usage`: one system line listing the three subcommands.
- [ ] **Step 5: manual check** (record in the commit message): `mur agent cli <agent>` → `/deep-research` renders the panel card; `/deep-research ask <q>` prints `run_id:` then iteration lines while the agent stays idle; `/deep-research stop` ends it with `outcome = stopped`; typing while it runs still works; the transcript sent to the agent contains **none** of these lines.
- [ ] **Step 6:** `cargo nextest run -p mur-core cli::` — green. fmt + clippy; commit `feat(murmur): /deep-research status|ask|stop with on-screen progress`.

---

## Task 6 — Skill v0.2.0 + docs trio (last)

**Files.** `mur-core/src/skills/mur_deep_research.yaml` (`version: 0.1.0` at line 2), `README.md` (`deep-research` block at `:712`, `:739–748`), docs site, product page.

- [ ] **Step 1: failing test** — find the built-in-skill seed test (`grep -rn "mur_deep_research" mur-core/src --include=*.rs`) and assert `version == "0.2.0"`, context mentions `fleet_run` with `wait: false` and `mur_job_status`, and does **not** mention `MUR_RUN_ID` (agents never set it; the tool does).
- [ ] **Step 2:** bump to `0.2.0`; document the shipped surface only: `fleet_run {fleet, goal, wait}` → `run_id:` line; poll `mur_job_status <run_id>`; states and `report being saved`; `/deep-research status|ask|stop` in murmur.
- [ ] **Step 3: docs trio.** `README.md` deep-research section: add `/deep-research` and the `wait:false` + `mur_job_status` flow. Docs site and product page sources are **not in this repo** (`mur-server/dashboard/docs-content/` does not exist here; `CLAUDE.md:158` points at https://app.mur.run/docs/core and /products/mur) — GitHub Manager files the follow-up in the site repo and links it from the PR.
- [ ] **Step 4:** `cargo nextest run -p mur-core skills` — green. Commit `docs(skill): mur-deep-research 0.2.0 + README`.

---

## Anchors (verified at `b30221c8` — so nobody re-explores)

| What | Where |
|---|---|
| `RunProgress` struct / `outcome` doc comment / `save` / `progress_path` / **`load()` → `(RunProgress, SystemTime)`** / `iteration_summary_line` | `mur-core/src/cmd/fleet/progress.rs:61–73`, `:66`, `:121–136`, `:138`, `:144–150`, `:151` |
| `PROGRESS_FILE`, `STALE_AFTER_SECS = 600` | `progress.rs:11`, `:14` |
| panel: `render_panel` / `render_progress(p, age)` / `cmd_panel` (age computed from mtime) / fixtures | `mur-core/src/cmd/deep_research/panel.rs:8`, `:46`, `:136–144`, tests `:152–258` (`progress_fixture` `:182`) |
| `DEFAULT_FLEET_NAME = "deep-research"` | `mur-core/src/cmd/deep_research/status.rs:11` |
| loop: `outcome_label` (nine terms) / `run_guarded` / progress mint `run_id` / first save / per-iter `RunKind::Fleet` record / per-iter spend save / terminal stamp / `cmd_fleet_run_loop` | `mur-core/src/cmd/fleet/loop_run.rs:334–345`, `:351`, `:418–435` (`:420`), `:435`, `:631–633`, `:673–678`, `:719–727`, `:734–748` |
| loop callers | `deep_research/run.rs:39–54` (`cmd_deep_research_run`), `dispatch.rs:319` |
| `cmd_ask` / `set_var` block / preflight `Err` points A–G / loop entry / report save / `extract_report` / `save_report` / tests | `mur-core/src/cmd/deep_research/ask.rs:70–156`, `:71–78`, `:85–88`,`:91`,`:95`,`:99–107`,`:114–125`,`:128–130`,`:133`, `:142`, `:147–153`, `:163`, `:203`, `:237–295` |
| DAG run-record creation (reference for what already exists — **do not duplicate**) | `mur-core/src/executor/dag.rs:1098–1125` |
| `run_status`: `RUN_SCHEMA` / `RunKind` / `State` / `Liveness` / `RunState` / `status_of` | `mur-core/src/run_status/mod.rs:29`, `:37`, `:46`, `:85`, `:131`, `:213` |
| `run_status::store` `save`/`update`/`load`/`list_ids`; `heartbeat::beat_once` (exists; unused here) | `store.rs:26`, `:93`, `:143`, `:156`; `heartbeat.rs:27` |
| `RUN_STATE_DIRS` already has `RUNS` (no carve-in work) | `mur-common/src/paths.rs:13`, `:19`; used at `mur-agent-runtime/src/sandbox/policy.rs:358`, guard test `:1185` |
| env-name precedent | `mur-common/src/identity.rs:174` `SIGNING_HANDOFF_ENV` |
| `fleet_run`: timeouts / `def` + schema / argv build / spawn env+`kill_on_drop(true)` / stdin handoff / timeout + `wait_with_output` / tests | `mur-agent-runtime/src/tools/fleet_run.rs:34–35`, `:84–113`, `:179–184`, `:205–216`, `:217–235`, `:237–248`, `:274–413` |
| `mur_job_status`: tool def / `AUTO_COMPRESS_SKIP` / arm / tests + `call_tool_in` | `mur-mcp-server/src/tools.rs:369–384`, `:388–393`, `:808–843`, `:968–1052` (`:981`, `:1002`, `:1038`) |
| murmur `SlashCmd` / `Panel(Vec<String>)` / `parse_slash` / `"panel"` arm / `other` arm / `parse_slash_*` tests / `push_system` / `push_shell` | `mur-core/src/cmd/agent/cli/app.rs:158`, `:186`, `:213`, `:268`, `:271`, `:1751`+, `:943`, `:1492` |
| murmur `help_text()` (grouped rows; `more` row) / `mod panel;` / submit path / Unknown→skill fallback / `!cmd` spawn → `ShellDone` / `route_shell_output` + `ShellRoute::{Start,Steer,Skip}` / `handle_slash` / `Panel` arm / `Open` arm / `Unknown` arm / `Note → push_system` / `ShellDone` handler | `mur-core/src/cmd/agent/cli/mod.rs:201–218` (`:213`), `:25`, `:1810–1826`, `:1815–1816`, `:1840–1848`, `:1938–1960`, `:2018`, `:2405`, `:2406–2416`, `:2417`, `:2560`, `:2584–2598` |
| parity tests: `help_coverage_tests` / `help_name` / `one_of_each` / `help_matches_the_composer_hint_and_the_skin_list` / `every_command_is_parsed_documented_and_offered` | `mod.rs:3291`, `:3300–3323`, `:3335–3368`, `:3377`, `:3400–3419` |
| completion: `Args` / `PANEL_TABS` / `COMMANDS` (`clear` `:210`, `effort` `:211`, `panel` `:224–228`) / `offers` (`#[cfg(test)]`) / `args_for` / `matched_skill` | `mur-core/src/cmd/agent/cli/complete.rs:148–161`, `:182–189`, `:201–241`, `:246–249`, `:253–258`, `:459` |
| `StreamMsg::Note(String)` | `mur-core/src/cmd/agent/cli/stream.rs:43` |
| skill | `mur-core/src/skills/mur_deep_research.yaml` (`version: 0.1.0`) |
| README deep-research section | `README.md:712`, `:739–748` |
| Corrections-section precedent | `docs/superpowers/plans/2026-09-07-murmur-secret-handoff.md:10–16` |

## Non-goals / out of scope

- A whole-loop `RunState`, loop heartbeats, or any change to `run_status::store` (decision 1).
- A new event channel, MCP notification, or GUI panel for progress — the progress file is the contract.
- Sandbox policy changes (decision 8).
- Routing any `/deep-research` output to the agent (decision 4).
- `schema_version` bump or migration (decision 5).
- Changing the nine loop outcome terms.
- Refactoring `cli/mod.rs`, `cli/app.rs`, or `mcp-server/tools.rs` below 800 lines — pre-existing; only "add no more" applies.

## Definition of done

- [ ] All six tasks ticked in this file, one commit each on `feat/deep-research-slash`
- [ ] `cargo nextest run -p mur-common -p mur-core -p mur-agent-runtime -p mur-mcp-server` green; fmt + clippy clean per crate
- [ ] Manual: `mur deep-research "x"` from a terminal prints `run_id:` first; `mur_job_status <id>` answers with a `progress:` line mid-run, `report being saved` in the terminal window, then the artifact path
- [ ] Manual: kill a worker before `mur deep-research "x"` so preflight fails → `mur_job_status <id>` says `state: failed` with the error
- [ ] Manual: `/deep-research status` in murmur renders the same numbers as `mur deep-research`; `/deep-research ask …` never produces an agent turn
- [ ] Docs trio: `README.md` updated in the PR; docs site + product page follow-up filed and linked (sources are external)
- [ ] PR opened against `main`, body links spec + this handoff, and this file gains a "Corrections found during execution" section in the style of `2026-09-07-murmur-secret-handoff.md:10–16`

## Ownership

- **rustsmith** builds Tasks 1–5 and the skill/README edits in Task 6, in order, one commit each.
- **qa** verifies each task's gate column, runs the manual checks in the definition of done, and signs off that no `/deep-research` output reaches the agent transcript.
- **repomanager** (GitHub Manager) opens the PR, files the docs-site/product-page follow-up, and merges after qa sign-off. No version bump/tag in this PR.
- **pm** ticks checkboxes here and owns the Corrections section.

## Report back

Per task: commit hash, test filter run + pass count, anything this handoff got wrong. At the end: PR URL and the corrections list.
