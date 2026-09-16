# Durable Monitor `rerun` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let an approved `rerun` action actually re-run a GitHub Actions run — and make the write capability something a user grants explicitly, not something a read-only monitor acquires by accident.

**Architecture:** The actions slice shipped a gate with nothing behind it: approve a `rerun` today and you get "this build cannot run it". The mechanism to fix that is already present — the adapter resolves `source.credential_ref` and sends it as a bearer token. What is missing is a *grant*. `MonitorSpec` has one credential reference and no read/write distinction, so a token supplied for watching would perform a write, gated only by the per-action approval. This plan adds an explicit write grant, refuses a spec that asks for a write without one **at creation time rather than at approval time**, and implements the executor on the same dedicated-OS-thread pattern the read path already uses.

**Tech Stack:** Rust 2024, `mur-monitor` (spec, risk table, action state), `mur-core` (adapters, executors, drain), `reqwest::blocking` on a dedicated thread, `mur_common::secret::SecretRef`.

**Spec:** `docs/superpowers/specs/2026-09-11-durable-monitor-design.md` — §風險政策, §混合處置策略, §安全與隱私, §錯誤處理.

## Global Constraints

- **The HITL gate approves an action, not a capability.** A user who supplied a credential for read-only monitoring never consented to MUR writing to their repository. The approval is per-action and per-occurrence; the grant is standing. They are different consents and this plan keeps them separate. This is the same distinction the notifications slice drew when it made desktop notifications opt-in: "a background daemon that starts popping OS banners the moment a user upgrades is a hostile default."
- **Refuse early, not late.** A spec whose action list needs a write must be rejected by `MonitorSpec::validate` — at `mur monitor add` — not accepted and then failed after a human presses approve. Approving something that was never going to run is worse than a clear refusal at creation.
- **Risk tier still comes from the fixed table keyed on the action type.** `mur_common::hitl::RiskTier`'s own doc: tiers are "NEVER LLM-asserted". `rerun` stays `Write`; the grant decides whether the spec is *allowed to ask*, never what tier it gets.
- **`reqwest::blocking::ClientBuilder::build` panics when dropped inside a Tokio runtime context.** `mur-core/src/monitor/adapters/github_actions.rs:135-142` documents this and runs the whole request on a dedicated OS thread with no ambient runtime. The predecessor slice shipped this panic once (`exit=101` on the first real `mur monitor add`). **The rerun POST follows the same pattern. This is the single most likely place to reintroduce it.**
- **Secrets never reach history, logs or notifications.** Store-bound strings redact *then* truncate, in that order (`mur_common::redact::redact_secrets`). A rerun's response body and any error must go through the same chokepoint.
- **`unknown` is a monitor problem, never a work failure.** A rerun that cannot be dispatched — 403, network, rate limit — is not evidence the work failed.
- Source files ≤ 800 lines. No hardcoded values in logic. **MUR uppercase** in user-visible prose; the CLI command `mur` stays lowercase. Comments in English; a `spec §<heading>` citation may keep the design doc's Chinese heading label.
- `cargo nextest`, never bare `cargo test`. `mur-core` needs `MUR_WEB_DIST` to build.

---

## Deliberately out of scope

- **`start_downstream` and `apply_known_remedy`.** They stay classified, gated and unrunnable. `start_downstream` is a deploy trigger whose blast radius is not a CI rerun's, and `apply_known_remedy` still has no catalogue anywhere in the repo or spec. The validation rule this plan adds is written so adding either later is a table entry, not a redesign.
- **AgentResolver.** Unchanged: its own plan.
- **Retrofitting the grant onto existing monitors.** Validation runs at `add`. A monitor already in the database with a `rerun` action keeps failing at approval exactly as it does today — it does not suddenly acquire a write grant, and it does not get retroactively refused. Task 4's docs say so.

---

## File structure

| File | Responsibility |
|---|---|
| `mur-monitor/src/spec.rs` (modify) | `Source::write_credential_ref`, a `SpecError` variant, and the validation rule. |
| `mur-monitor/src/action/risk.rs` (modify) | `needs_write_grant(action_type) -> bool` beside `classify`. One table, two questions. |
| `mur-core/src/monitor/adapters/github_actions.rs` (modify) | `rerun(reference, write_credential_ref) -> Result<String, String>` on the dedicated-thread pattern. |
| `mur-core/src/monitor/actions/rerun.rs` (new) | The `ActionExecutor` impl: resolve the grant, call the adapter, redact the result. |
| `mur-core/src/monitor/actions/mod.rs` (modify) | Register it in `executor_for`. |
| `CLAUDE.md`, `README.md` (modify) | What the grant is, why it is separate, and what happens to a spec without it. |

---

### Task 1: The write grant, and refusing a spec that asks for a write without one

**Files:**
- Modify: `mur-monitor/src/spec.rs`, `mur-monitor/src/action/risk.rs`

**Interfaces:**
- Produces: `Source::write_credential_ref: Option<String>`, `SpecError::WriteGrantMissing(String)`, `risk::needs_write_grant(&str) -> bool`.
- Consumes: `mur_common::secret::SecretRef` (format validation only — never resolved here), the existing `risk::classify`.

`needs_write_grant` lives beside `classify` because they answer two questions about the same table and must not drift apart. It is **not** `classify(v) > Read`: `start_downstream` and `apply_known_remedy` are above `Read` and have no executor, and requiring a grant for them would refuse specs that are accepted today. It names the verbs this build can actually execute as an external write — today, `rerun` alone.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_spec_with_rerun_and_no_write_grant_is_refused() {
    // Refused at `add`, not after a human presses approve. Approving
    // something that was never going to run is worse than a clear refusal.
    let s = spec_with_actions("on_failure:\n    - type: rerun", None);
    match s.validate() {
        Err(SpecError::WriteGrantMissing(v)) => assert_eq!(v, "rerun"),
        other => panic!("must refuse, got {other:?}"),
    }
}

#[test]
fn the_same_spec_with_a_write_grant_validates() {
    let s = spec_with_actions("on_failure:\n    - type: rerun", Some("env:GH_WRITE"));
    assert!(s.validate().is_ok(), "{:?}", s.validate());
}

#[test]
fn a_read_only_spec_needs_no_write_grant() {
    // The property that must not regress: every monitor that exists today
    // keeps validating without touching its YAML.
    let s = spec_with_actions("on_failure:\n    - type: collect_logs", None);
    assert!(s.validate().is_ok(), "{:?}", s.validate());
}

#[test]
fn a_gated_verb_this_build_cannot_run_still_needs_no_grant() {
    // `start_downstream` is above Read but has no executor. Requiring a
    // grant for it would refuse specs that validate today, for a write that
    // cannot happen. Out of scope means out of scope.
    let s = spec_with_actions("on_failure:\n    - type: start_downstream", None);
    assert!(s.validate().is_ok(), "{:?}", s.validate());
}

#[test]
fn a_malformed_write_grant_is_refused_without_echoing_it() {
    // The predecessor slice leaked a pasted PAT through a parse error that
    // embedded its input. The message names the accepted schemes and never
    // the value.
    let s = spec_with_actions("on_failure:\n    - type: rerun", Some("ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"));
    let e = s.validate().unwrap_err().to_string();
    assert!(!e.contains("ghp_"), "must not echo the value: {e}");
    assert!(e.contains("env:") || e.contains("keychain:"), "must name the schemes: {e}");
}

#[test]
fn needs_write_grant_names_only_verbs_this_build_executes_as_a_write() {
    assert!(risk::needs_write_grant("rerun"));
    for v in ["notify", "collect_logs", "reschedule_monitor", "start_downstream", "apply_known_remedy"] {
        assert!(!risk::needs_write_grant(v), "{v}");
    }
    // Unknown verbs are already refused by `SpecError::Action`; a grant
    // question about them never arises.
    assert!(!risk::needs_write_grant("nonsense"));
}
```

- [ ] **Step 2: Run to verify they fail.** `cargo nextest run -p mur-monitor -E 'test(/grant/) or test(/write/)'`

- [ ] **Step 3: Implement.**

```rust
// spec.rs, on `Source`:
    /// A second `SecretRef`, for actions that WRITE to the source. Separate
    /// from `credential_ref` on purpose: the HITL gate approves an action,
    /// not a capability, and a user who supplied a credential so MUR could
    /// watch a run never consented to MUR restarting it. Absent means this
    /// monitor may only read — and a spec whose actions need a write is
    /// refused at creation rather than failing after someone approves it.
    #[serde(default)]
    pub write_credential_ref: Option<String>,
```

```rust
// spec.rs, in `validate`, after the existing credential_ref check:
        if let Some(c) = &self.write_credential_ref {
            SecretRef::from_str(c)
                .map_err(|_| SpecError::Credential("invalid secret reference format".into()))?;
        }
        if self.write_credential_ref.is_none()
            && let Some(v) = self.all_action_types().find(|v| risk::needs_write_grant(v))
        {
            return Err(SpecError::WriteGrantMissing(v.to_string()));
        }
```

`all_action_types` is a small private helper over `on_success`, `on_failure` and `on_unknown`. The `SpecError` variant:

```rust
    #[error(
        "action `{0}` writes to the source, so the spec needs a `source.write_credential_ref` \
         (env:NAME, keychain:service/account, file:PATH or cmd:...) — `credential_ref` is read-only"
    )]
    WriteGrantMissing(String),
```

`risk.rs`:

```rust
/// Does this verb perform an external WRITE that this build can actually
/// execute? Deliberately not `classify(v) > Read`: `start_downstream` and
/// `apply_known_remedy` are above `Read` and have no executor, so demanding
/// a grant for them would refuse specs that validate today for a write that
/// cannot happen. Adding an executor for either means adding it here too —
/// they are two questions about one table and must not drift.
pub fn needs_write_grant(action_type: &str) -> bool {
    matches!(action_type, "rerun")
}
```

- [ ] **Step 4: Run to verify they pass.**

- [ ] **Step 5: Mutation-check the refusal.** Make `needs_write_grant` return `false` for everything and confirm `a_spec_with_rerun_and_no_write_grant_is_refused` goes red. **Verify the mutation is present in the code** (`grep` for it) before trusting the result — a heredoc that silently failed to apply has produced a meaningless PASS in this project before.

- [ ] **Step 6: Commit**

```bash
git add mur-monitor/src/spec.rs mur-monitor/src/action/risk.rs
git commit -F - <<'MSG'
feat(monitor): a write grant, separate from the read credential

The HITL gate approves an action, not a capability. A user who supplied a
credential so MUR could watch a run never consented to MUR restarting it, so
a spec whose actions write to the source now needs its own
`source.write_credential_ref` — and is refused at creation rather than after
somebody presses approve.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
MSG
```

---

### Task 2: The adapter's rerun POST

**Files:**
- Modify: `mur-core/src/monitor/adapters/github_actions.rs`

**Interfaces:**
- Produces: `GithubActionsAdapter::rerun(&self, reference: &str, write_credential_ref: Option<&str>) -> Result<String, String>`.
- Consumes: the existing `parse_reference`, `USER_AGENT`, `self.timeout`.

The endpoint is `POST /repos/{owner}/{repo}/actions/runs/{run_id}/rerun-failed-jobs`. **Failed jobs, not the whole run**: a monitor reruns because something failed, and re-running green jobs costs minutes and can re-trigger their side effects.

- [ ] **Step 1: Write the failing tests** (against a local `TcpListener` stub, the shape this file's existing tests already use)

```rust
#[test]
fn a_successful_rerun_reports_which_run_it_restarted() {
    let srv = stub(201, "");
    let a = adapter_pointing_at(&srv);
    let out = a.rerun("o/r/12345", Some("env:TEST_TOKEN")).unwrap();
    assert!(out.contains("12345"), "must name the run: {out}");
}

#[test]
fn a_403_says_the_token_lacks_write_scope_and_does_not_echo_it() {
    let srv = stub(403, r#"{"message":"Resource not accessible by personal access token"}"#);
    let a = adapter_pointing_at(&srv);
    let e = a.rerun("o/r/12345", Some("env:TEST_TOKEN")).unwrap_err();
    assert!(e.contains("actions:write"), "must name the missing scope: {e}");
    assert!(!e.contains(TEST_TOKEN_VALUE), "must not echo the token: {e}");
}

#[test]
fn a_rerun_without_a_grant_refuses_before_any_request() {
    // Belt to Task 1's braces: validation should have caught it, but the
    // adapter must not fall back to an unauthenticated POST.
    let srv = stub(201, "");
    let a = adapter_pointing_at(&srv);
    let e = a.rerun("o/r/12345", None).unwrap_err();
    assert!(e.contains("write_credential_ref"), "{e}");
    assert_eq!(srv.hits(), 0, "must not have called GitHub at all");
}

#[test]
fn an_unresolvable_grant_refuses_without_echoing_the_reference() {
    let a = adapter_pointing_at(&stub(201, ""));
    let e = a.rerun("o/r/12345", Some("env:DEFINITELY_NOT_SET")).unwrap_err();
    assert!(e.contains("could not be resolved"), "{e}");
}

#[test]
fn the_request_is_a_post_to_rerun_failed_jobs() {
    // Re-running the whole run costs minutes and can re-fire the side
    // effects of jobs that already succeeded.
    let srv = stub(201, "");
    let a = adapter_pointing_at(&srv);
    a.rerun("o/r/12345", Some("env:TEST_TOKEN")).unwrap();
    let req = srv.last_request();
    assert_eq!(req.method, "POST");
    assert!(req.path.ends_with("/actions/runs/12345/rerun-failed-jobs"), "{}", req.path);
}
```

- [ ] **Step 2: Run to verify they fail.**

- [ ] **Step 3: Implement.** Copy the shape of `fetch` exactly — including `std::thread::spawn` and the comment explaining why. Do not call `reqwest::blocking` on the caller's thread; do not reach for the async client. Resolve the grant with `SecretRef::from_str(...).resolve_to_string_blocking()`, same as `observe` does for the read credential. Map the status: `201`/`204` → `Ok`, `403` → an error naming `actions:write`, `404` → run not found, anything else → the status and a redacted body.

- [ ] **Step 4: Run to verify they pass.**

- [ ] **Step 5: Confirm the blocking-client defence is intact.** The test suite cannot tell a correct bridge from one that panics in the daemon — every test here runs on a thread with no ambient runtime. Read the code and confirm `rerun` spawns its own thread exactly as `fetch` does, and say so in the report.

- [ ] **Step 6: Commit** — `feat(monitor): the GitHub adapter can rerun failed jobs`.

---

### Task 3: The executor, and what happens after a rerun

**Files:**
- Create: `mur-core/src/monitor/actions/rerun.rs`
- Modify: `mur-core/src/monitor/actions/mod.rs`

**Interfaces:**
- Consumes: `ActionExecutor`, `ActionCtx { store, row, now, registry }`, Task 2's `GithubActionsAdapter::rerun`.
- Produces: `pub struct Rerun;` registered in `executor_for` as `"rerun"`.

Two things this task must decide, and the plan decides them here so no implementer has to guess:

**The monitor does not follow the new run.** A rerun mints a *new* GitHub run id, and the spec's child-cycle machinery (§混合處置策略 step 5, `monitor_cycles.parent_cycle_id`) is out of scope and unwritten. So the executor records what it started and the monitor settles as it already had. Saying "rerun dispatched, watching run 999" when nothing watches it would be a lie in exactly the register-or-outbox shape Part C exists to prevent. The result string names the new run and says plainly that it is **not** being monitored, and the docs repeat it.

**A failed rerun is a failed remediation, not a failed monitor.** `Err` from the adapter → `ActionState::Failed` and the drain's existing `remediation_failed` event. The work's own outcome is untouched: the monitor already settled, and a rerun that could not be dispatched says nothing about whether the build passed.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_successful_rerun_records_the_new_run_and_says_it_is_unwatched() {
    let (_d, s, row) = fixture_with_write_grant();
    let ctx = ActionCtx { store: &s, row: &row, now: t0(), registry: &reg() };
    let out = executor_for("rerun").unwrap().run(&ctx, &Default::default()).unwrap();
    assert!(out.contains("not monitored"), "must not imply we are watching it: {out}");
}

#[test]
fn a_rerun_failure_is_an_action_failure_not_a_work_failure() {
    let (_d, s, row) = fixture_where_rerun_403s();
    let ctx = ActionCtx { store: &s, row: &row, now: t0(), registry: &reg() };
    let e = executor_for("rerun").unwrap().run(&ctx, &Default::default()).unwrap_err();
    assert!(e.contains("actions:write"), "{e}");
    // The monitor's own verdict is untouched: a rerun we could not dispatch
    // says nothing about whether the build passed.
    assert_eq!(s.get(&row.id).unwrap().unwrap().outcome, row.outcome);
}

#[test]
fn the_result_never_carries_the_token() {
    let (_d, s, row) = fixture_where_rerun_403s();
    let ctx = ActionCtx { store: &s, row: &row, now: t0(), registry: &reg() };
    let e = executor_for("rerun").unwrap().run(&ctx, &Default::default()).unwrap_err();
    assert!(!e.contains(TEST_TOKEN_VALUE), "{e}");
}

#[test]
fn rerun_is_registered_and_still_classified_write() {
    assert_eq!(executor_for("rerun").unwrap().verb(), "rerun");
    assert_eq!(risk::classify("rerun"), RiskTier::Write, "having an executor must not lower its tier");
}

#[test]
fn a_source_that_is_not_github_refuses_rather_than_pretending() {
    // `rerun` on a `mur_run` monitor has nothing to call.
    let (_d, s, row) = fixture_with_source(SourceType::MurRun);
    let ctx = ActionCtx { store: &s, row: &row, now: t0(), registry: &reg() };
    let e = executor_for("rerun").unwrap().run(&ctx, &Default::default()).unwrap_err();
    assert!(e.contains("github_actions"), "{e}");
}
```

- [ ] **Step 2–4: Red, implement, green.** Register `"rerun" => Some(&RERUN)` in `executor_for`. The drain needs no change: a verb with an executor stops reaching the no-executor arm on its own.

- [ ] **Step 5: Mutation-check the tier.** Change `classify("rerun")` to `Read` and confirm `rerun_is_registered_and_still_classified_write` goes red — an executor appearing must never quietly make a verb auto-executable.

- [ ] **Step 6: Commit** — `feat(monitor): an approved rerun actually reruns`.

---

### Task 4: Docs

**Files:** `CLAUDE.md` (the `mur monitor` bullet), `README.md` ("Durable monitors").

Four things must land, and the first is the one a user will hit:

1. **`rerun` needs `source.write_credential_ref`**, and a spec that asks for it without one is refused by `mur monitor add` — not at approval. Show the field.
2. **Why it is separate from `credential_ref`**: the approval gate covers an action, not a capability. One sentence, no lecture.
3. **A rerun's new run is not monitored.** Register a second monitor for it if you want it watched.
4. **A monitor that already exists is unaffected.** Validation runs at `add`; nothing retroactively refuses or upgrades a monitor already in the database.

The CLAUDE.md bullet's out-of-scope clause needs correcting **for the fourth time on this feature** — `rerun` leaves it, `start_downstream` and `apply_known_remedy` and AgentResolver stay. Read the current sentence before editing; do not assume what it says.

- [ ] **Step 1:** Both edits.
- [ ] **Step 2:** `mur verify --file README.md` and `--file CLAUDE.md`. Three pre-existing complaints on CLAUDE.md are known (`openspec/changes/`, `queue/events.jsonl`) — do not "fix" them, and do not add a fourth.
- [ ] **Step 3: Commit** — `docs(monitor): the write grant and what a rerun does not do`.

---

## Self-review

**Spec coverage.** §風險政策's low-risk list includes 「對明確標記 flaky 且未達上限的同一 CI job 執行 rerun」. This plan does **not** implement the flaky-marking, so `rerun` stays `Write` and keeps asking — classifying low on a precondition nothing can evaluate is how an unattended process does something nobody approved. Recorded in Task 1's table comment and unchanged from the actions slice.

§混合處置策略 step 5 (re-observation, child cycles) remains unimplemented and is now *reachable* for the first time: a rerun genuinely starts new work. The plan's answer is to say so rather than pretend, and Task 3 makes that a test. Whoever plans child cycles starts here.

**Placeholder scan.** Task 2's `stub`/`adapter_pointing_at`/`srv.hits()` helpers are named but not written out: `github_actions.rs` already has a local HTTP stub its existing tests use, and the implementer is told to follow it rather than invent a second one. If that helper turns out not to exist, that is a NEEDS_CONTEXT, not an invitation to build a mock framework.

**Type consistency.** `needs_write_grant` (Task 1) is consumed by Task 1's own validation only. `GithubActionsAdapter::rerun` (Task 2) is consumed by Task 3. `ActionCtx`'s four fields are as the actions slice left them.

**Known ceilings, named.** A rerun is fire-and-forget: `Ok` means GitHub accepted the POST, not that the jobs passed. The grant is per-monitor, so a spec with a write grant can use it for any future write verb added to `needs_write_grant` — which is why that function names verbs explicitly instead of testing a tier.
