# Unified Channel v3a — Workflow-DAG Mode 2 over a Channel — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the shipped DAG executor (`mur-core/src/executor/dag.rs`) optionally run a `category: Workflow` Skill **over a Channel**, emitting an attributed, durable event trail (`StateChange` → `ToolCall`/`ToolResult` per step → `StateChange`) into `~/.mur/channels/<id>/events.jsonl` that v2's Work view renders — with **no** delegation, **no** HITL hardening, and **no** runtime/dispatcher changes.

**Architecture:** A single OS process (the executor) is the **sole writer** of channel events (per the v3 spec Decision A / trust-model invariant). The executor opens `mur_channel::ChannelService` and appends events via the existing `events.lock`-serialized `append_event` (safe for the concurrent same-rank steps). Lifecycle (`StateChange`) is written from the single `execute_dag` context; per-step `ToolCall`/`ToolResult` are written from `execute_step` (so retries naturally produce one event pair per attempt). Workflows containing a `needs_approval` step are **refused** when run over a channel (fail-closed) — the interactive channel-gated approval is v3c, and v3a must not inherit the executor's non-TTY silent-skip-as-success. The `ChannelEvent` schema gains two optional, `None`-valued fields (`sig`, `key_version`) as forward-compat reservation for v3d signing (no `CHANNEL_SCHEMA_VERSION` bump — the file's own rule).

**Tech Stack:** Rust — `mur-common` (channel types), `mur-channel` (store/service), `mur-core` (DAG executor + `mur workflow` CLI). All three are workspace members.

**Scope guardrails (from `docs/superpowers/specs/2026-06-16-unified-channel-v3-design.md` §7 v3a + §8):**
- v3a runs the **existing local command/intent steps**; it does **not** dial specialists (v3b) and does **not** add risk-tiering, hash-pinning, or the `hitl::gate` (v3c).
- Actor for executor-emitted events is `ChannelActor::System` (the run process; not a conversational agent).
- `idempotency_key` is left `None` in v3a (dedup is owned by v3c); v3b/v3c add deterministic keys + dedup.
- Read order is always `seq` (never `ts`).

**Key facts locked during exploration (do not re-derive):**
- `execute_dag(mur_home, skill_name, procedure, opts)` is the entry (`dag.rs:345`); called from `mur-core/src/cmd/workflow.rs:197`.
- The per-rank spawn (`dag.rs:388-405`) rebuilds a `DagExecOptions` (`opts_clone`) for each spawned `execute_step`; it carries **no** channel handle today.
- `execute_step` (`dag.rs:171`) runs command-mode (`sh -c`) or intent-mode steps; retries are driven by the collector loop calling `execute_step` again (`dag.rs:499`).
- `ChannelEvent` is constructed as a struct literal in exactly two places: `mur-channel/src/store.rs:103` and the `mur-common/src/channel.rs` test at `:160`. Adding `#[serde(default)]` fields needs both literals updated; deserialization of old rows stays clean.
- `ChannelService::append_message` (`service.rs:62`) does store.append_event + manifest `updated_at` bump + index upsert. `ChannelState` already has `Working/Completed/Failed`. `mur-core` already depends on `mur-channel` (`cli/persist.rs`).
- `cargo test --workspace` is flaky for `mur-core` (use `cargo nextest run`); 4 pre-existing `conversations::summarize::rollup` failures are unrelated.

---

## File Structure

**Modified:**
- `mur-common/src/channel.rs` — add optional `sig`/`key_version` to `ChannelEvent` + canonical sign-input doc-comment; update the test literal.
- `mur-channel/src/store.rs` — set `sig: None, key_version: None` in the `append_event` literal.
- `mur-channel/src/service.rs` — add `append` (structured payload) + `transition` (state + `StateChange` event) + `create_for_workflow`; refactor `append_message` to delegate to `append`.
- `mur-core/src/executor/dag.rs` — `DagExecOptions.channel_id`; `needs_approval` fail-closed pre-check; thread `mur_home` into `execute_step`; emit `ToolCall`/`ToolResult` per step; emit start/end `StateChange`.
- `mur-core/src/cmd/workflow.rs` — `--channel <id>` / `--channel-new` flags wired into `DagExecOptions`.

No new files. No `mur-agent-runtime` changes.

---

## Task 1: Reserve `ChannelEvent` signing fields (schema reservation)

**Files:**
- Modify: `mur-common/src/channel.rs:135-146` (struct + doc), `:160-167` (test literal)
- Modify: `mur-channel/src/store.rs:103-110` (constructor)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `mur-common/src/channel.rs`:

```rust
    #[test]
    fn event_omits_sig_fields_when_absent_and_reads_old_rows() {
        // New event with no signature serializes WITHOUT the sig/key_version keys
        // (skip_serializing_if), and an OLD row lacking them still deserializes.
        let ev = ChannelEvent {
            seq: 1,
            ts: Utc::now(),
            actor: ChannelActor::System,
            kind: EventKind::ToolCall,
            payload: serde_json::json!({ "step_id": "s0" }),
            idempotency_key: None,
            sig: None,
            key_version: None,
        };
        let line = serde_json::to_string(&ev).unwrap();
        assert!(!line.contains("\"sig\""), "absent sig must be omitted: {line}");
        assert!(!line.contains("key_version"), "absent key_version must be omitted");

        // An old row (pre-field) deserializes with sig/key_version defaulting to None.
        let old = r#"{"seq":0,"ts":"2026-06-16T00:00:00Z","actor":{"kind":"system"},"kind":"note","payload":{}}"#;
        let back: ChannelEvent = serde_json::from_str(old).unwrap();
        assert_eq!(back.sig, None);
        assert_eq!(back.key_version, None);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-common channel::tests::event_omits_sig_fields_when_absent_and_reads_old_rows`
Expected: FAIL — `ChannelEvent` has no field `sig` / `key_version`.

- [ ] **Step 3: Add the fields + doc-comment**

In `mur-common/src/channel.rs`, replace the `ChannelEvent` struct (`:135-146`) with:

```rust
/// One append-only line in `~/.mur/channels/<id>/events.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelEvent {
    pub seq: u64,
    pub ts: DateTime<Utc>,
    pub actor: ChannelActor,
    pub kind: EventKind,
    #[serde(default)]
    pub payload: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// Detached signature over the canonical event bytes (v3d). RESERVED — always
    /// `None` until per-event Ed25519 signing lands. Canonical sign-input is the
    /// JSON of `{channel_id, actor, kind, payload, idempotency_key}` EXCLUDING the
    /// store-assigned `seq` and `ts` (the signer does not know `seq`; the store
    /// restamps `ts` under the append lock). This reservation keeps v3d an
    /// additive change rather than a `CHANNEL_SCHEMA_VERSION` bump.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig: Option<String>,
    /// Identity key version that produced `sig`, resolved through the rotation
    /// chain at verify time (v3d). RESERVED — always `None` until signing lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_version: Option<u32>,
}
```

Update the existing `event_round_trips` test literal (`:160-167`) to add the two fields:

```rust
            idempotency_key: None,
            sig: None,
            key_version: None,
        };
```

- [ ] **Step 4: Update the store constructor**

In `mur-channel/src/store.rs`, the `ChannelEvent` literal in `append_event` (`:103-110`) becomes:

```rust
        let ev = ChannelEvent {
            seq: next_seq,
            ts: Utc::now(),
            actor,
            kind,
            payload,
            idempotency_key,
            sig: None,
            key_version: None,
        };
```

- [ ] **Step 5: Run tests to verify they pass**

Run:
```bash
cargo test -p mur-common channel::
cargo test -p mur-channel store::
```
Expected: PASS (new test + existing `event_round_trips`, `append_assigns_monotonic_seq`).

- [ ] **Step 6: Commit**

```bash
git add mur-common/src/channel.rs mur-channel/src/store.rs
git commit -m "feat(channel): reserve ChannelEvent sig/key_version for v3d signing

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 2: `ChannelService::append` + `transition` + `create_for_workflow`

**Files:**
- Modify: `mur-channel/src/service.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `service.rs`:

```rust
    #[test]
    fn append_structured_and_transition() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("deploy").unwrap();
        assert_eq!(ch.state, ChannelState::Working);
        assert!(ch.participants.is_empty(), "workflow channel has no agent participant");

        // Structured append (arbitrary payload + kind), not just text.
        svc.append(
            &ch.id,
            ChannelActor::System,
            EventKind::ToolCall,
            serde_json::json!({ "step_id": "s0", "command": "echo hi" }),
            None,
        )
        .unwrap();

        // Transition writes a StateChange event AND updates the manifest.
        let ev = svc
            .transition(&ch.id, ChannelState::Completed, ChannelActor::System)
            .unwrap();
        assert_eq!(ev.kind, EventKind::StateChange);
        assert_eq!(ev.payload["from"], "working");
        assert_eq!(ev.payload["to"], "completed");
        assert_eq!(svc.store().load_manifest(&ch.id).unwrap().state, ChannelState::Completed);

        let evs = svc.load_events(&ch.id).unwrap();
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].kind, EventKind::ToolCall);
        assert_eq!(evs[0].payload["command"], "echo hi");
        assert_eq!(evs[1].kind, EventKind::StateChange);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-channel service::tests::append_structured_and_transition`
Expected: FAIL — no `append` / `transition` / `create_for_workflow` methods.

- [ ] **Step 3: Implement the three methods + refactor `append_message`**

In `service.rs`, add inside `impl ChannelService` (after `create_for_agent`, before `append_message`):

```rust
    /// Create a fresh channel for a workflow run: owner = local human, no agent
    /// participant, title/goal derived from the skill name. Used by `mur workflow
    /// run --channel-new`.
    pub fn create_for_workflow(&self, skill_name: &str) -> Result<Channel> {
        let now = Utc::now();
        let ch = Channel {
            v: CHANNEL_SCHEMA_VERSION,
            id: uuid::Uuid::now_v7().to_string(),
            title: format!("workflow: {skill_name}"),
            goal: Goal {
                statement: format!("run workflow `{skill_name}`"),
                acceptance_criteria: vec![],
            },
            state: ChannelState::Working,
            owner: ChannelActor::local_human(),
            participants: vec![Participant {
                actor: ChannelActor::local_human(),
                role: ParticipantRole::Owner,
                joined_at: now,
            }],
            created_at: now,
            updated_at: now,
        };
        self.store.create(&ch)?;
        self.index.upsert(&ch)?;
        Ok(ch)
    }

    /// Append an event with an arbitrary structured payload, bumping the
    /// manifest's `updated_at` + index (the structured sibling of
    /// `append_message`). Used by the executor for ToolCall/ToolResult/Note.
    pub fn append(
        &self,
        channel_id: &str,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
    ) -> Result<ChannelEvent> {
        let ev = self
            .store
            .append_event(channel_id, actor, kind, payload, idempotency_key)?;
        if let Ok(mut ch) = self.store.load_manifest(channel_id) {
            ch.updated_at = ev.ts;
            self.store.save_manifest(&ch)?;
            self.index.upsert(&ch)?;
        }
        Ok(ev)
    }

    /// Transition a channel's lifecycle state: read current state, append a
    /// `StateChange` event `{from, to}`, then update the manifest + index. The
    /// event log is the source of truth; the manifest `state` is the cache v2's
    /// Work view reads.
    pub fn transition(
        &self,
        channel_id: &str,
        new_state: ChannelState,
        actor: ChannelActor,
    ) -> Result<ChannelEvent> {
        let from = self
            .store
            .load_manifest(channel_id)
            .map(|c| c.state)
            .unwrap_or(ChannelState::Working);
        let payload = serde_json::json!({
            "from": state_str(from),
            "to": state_str(new_state),
        });
        let ev = self
            .store
            .append_event(channel_id, actor, EventKind::StateChange, payload, None)?;
        if let Ok(mut ch) = self.store.load_manifest(channel_id) {
            ch.state = new_state;
            ch.updated_at = ev.ts;
            self.store.save_manifest(&ch)?;
            self.index.upsert(&ch)?;
        }
        Ok(ev)
    }
```

Refactor `append_message` (`:62-83`) to delegate to `append`:

```rust
    /// Append a message event and bump the manifest's `updated_at` + index.
    pub fn append_message(
        &self,
        channel_id: &str,
        actor: ChannelActor,
        kind: EventKind,
        text: &str,
        task_id: Option<&str>,
    ) -> Result<ChannelEvent> {
        let mut payload = serde_json::json!({ "text": text });
        if let Some(t) = task_id {
            payload["task_id"] = serde_json::Value::String(t.to_string());
        }
        self.append(channel_id, actor, kind, payload, None)
    }
```

Add this free helper at the bottom of `service.rs` (above `#[cfg(test)]`):

```rust
/// Kebab string for a `ChannelState` (matches the serde rename), without pulling
/// in a full serde round-trip at each call site.
fn state_str(s: ChannelState) -> &'static str {
    match s {
        ChannelState::Submitted => "submitted",
        ChannelState::Working => "working",
        ChannelState::InputRequired => "input-required",
        ChannelState::Completed => "completed",
        ChannelState::Failed => "failed",
        ChannelState::Canceled => "canceled",
        ChannelState::Rejected => "rejected",
        ChannelState::Stale => "stale",
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-channel service::`
Expected: PASS (new test + existing `create_append_resume_roundtrip`, which still works since `append_message` delegates).

- [ ] **Step 5: Commit**

```bash
git add mur-channel/src/service.rs
git commit -m "feat(channel): ChannelService append/transition/create_for_workflow

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 3: `DagExecOptions.channel_id` + `needs_approval` fail-closed

**Files:**
- Modify: `mur-core/src/executor/dag.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `dag.rs`:

```rust
    #[tokio::test]
    async fn channel_run_refuses_needs_approval() {
        let tmp = tempfile::TempDir::new().unwrap();
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("approve-wf").unwrap();

        let mut s = step("s0", &[], Some("echo hi"));
        s.needs_approval = true;
        let proc = Procedure { variables: vec![], steps: vec![s] };

        let opts = DagExecOptions { channel_id: Some(ch.id.clone()), ..Default::default() };
        let err = execute_dag(tmp.path(), "approve-wf", &proc, &opts)
            .await
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("HITL"),
            "approval-bearing workflow over a channel must be refused (v3c), got: {err:#}"
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core executor::dag::tests::channel_run_refuses_needs_approval`
Expected: FAIL — `DagExecOptions` has no field `channel_id`.

- [ ] **Step 3: Add the field + default + pre-check**

In `dag.rs`, add to `DagExecOptions` (after `trigger`, `:36`):

```rust
    /// When set, the executor runs OVER this channel: it emits attributed
    /// lifecycle + per-step events into `~/.mur/channels/<id>/`. v3a only —
    /// approval-bearing workflows are refused (see `execute_dag`).
    pub channel_id: Option<String>,
```

In the `Default` impl (`:39-49`), add `channel_id: None,`.

Add the import at the top of `dag.rs` (after the existing `mur_common` uses):

```rust
use mur_common::channel::{ChannelActor, ChannelState, EventKind};
use mur_channel::ChannelService;
```

In `execute_dag`, immediately after `let graph = build_dag(&procedure.steps)?;` (`:353`), add the fail-closed guard:

```rust
    // v3a: running over a channel is non-interactive; an approval-bearing step
    // would hit the executor's non-TTY silent-skip-as-success landmine. Refuse
    // up front — interactive channel-gated approval lands in v3c.
    if opts.channel_id.is_some() && procedure.steps.iter().any(|s| s.needs_approval) {
        anyhow::bail!(
            "workflow `{skill_name}` has a needs_approval step — running over a channel \
             requires HITL (v3c); run it without --channel, or wait for v3c"
        );
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core executor::dag::tests::channel_run_refuses_needs_approval`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/executor/dag.rs
git commit -m "feat(executor): DagExecOptions.channel_id + needs_approval fail-closed

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 4: Emit attributed step + lifecycle events

**Files:**
- Modify: `mur-core/src/executor/dag.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `dag.rs`:

```rust
    #[tokio::test]
    async fn channel_run_emits_attributed_event_trail() {
        let tmp = tempfile::TempDir::new().unwrap();
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("trail-wf").unwrap();

        let proc = Procedure {
            variables: vec![],
            steps: vec![
                step("s0", &[], Some("echo zero")),
                step("s1", &["s0"], Some("echo one")),
            ],
        };
        let opts = DagExecOptions { channel_id: Some(ch.id.clone()), ..Default::default() };
        let out = execute_dag(tmp.path(), "trail-wf", &proc, &opts).await.unwrap();
        assert_eq!(out.exit_code, 0);

        let evs = svc.load_events(&ch.id).unwrap();
        let kinds: Vec<_> = evs.iter().map(|e| e.kind).collect();
        // start StateChange(Working) + 2×(ToolCall,ToolResult) + end StateChange(Completed)
        assert_eq!(kinds.first(), Some(&EventKind::StateChange));
        assert_eq!(kinds.last(), Some(&EventKind::StateChange));
        assert_eq!(evs.iter().filter(|e| e.kind == EventKind::ToolCall).count(), 2);
        assert_eq!(evs.iter().filter(|e| e.kind == EventKind::ToolResult).count(), 2);
        // every executor event is attributed to System (sole writer, v3a)
        assert!(evs.iter().all(|e| e.actor == ChannelActor::System));
        // final manifest state is Completed
        assert_eq!(svc.store().load_manifest(&ch.id).unwrap().state, ChannelState::Completed);
        // a ToolResult carries the step's exit_code
        let tr = evs.iter().find(|e| e.kind == EventKind::ToolResult).unwrap();
        assert_eq!(tr.payload["exit_code"], 0);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core executor::dag::tests::channel_run_emits_attributed_event_trail`
Expected: FAIL — no events emitted yet (only the manifest from `create_for_workflow`; `load_events` is empty → assertions fail).

- [ ] **Step 3: Add the channel-emit helper**

In `dag.rs`, add this free helper (after the `StepResult` struct, before `execute_step`):

```rust
/// Best-effort: append one executor event to the channel as `System` (the sole
/// writer in v3a). Opens the service per call (channel writes are infrequent vs
/// step work, and this avoids holding a `!Sync` SQLite handle across `.await`).
/// Failures are logged, never fatal to the run.
fn emit_channel(mur_home: &Path, channel_id: &str, kind: EventKind, payload: serde_json::Value) {
    match ChannelService::open(mur_home) {
        Ok(svc) => {
            if let Err(e) = svc.append(channel_id, ChannelActor::System, kind, payload, None) {
                eprintln!("  ⚠ channel emit failed: {e:#}");
            }
        }
        Err(e) => eprintln!("  ⚠ channel open failed: {e:#}"),
    }
}
```

- [ ] **Step 4: Emit `ToolCall`/`ToolResult` from `execute_step`**

Change the `execute_step` signature (`:171-175`) to take `mur_home`:

```rust
async fn execute_step(
    step: &ProcedureStep,
    opts: &DagExecOptions<'_>,
    step_index: usize,
    mur_home: &Path,
) -> StepResult {
```

At the very top of `execute_step` (after `let start = ...;`), emit the `ToolCall`:

```rust
    let sid = step.id.clone().unwrap_or_else(|| step_index.to_string());
    if let Some(cid) = opts.channel_id.as_deref() {
        emit_channel(
            mur_home,
            cid,
            EventKind::ToolCall,
            serde_json::json!({
                "step_id": sid,
                "description": step.description,
                "command": step.command,
                "tool": step.tool,
            }),
        );
    }
```

Then wrap every `return StepResult { .. }` and the final fall-through so a `ToolResult` is emitted with the outcome. The cleanest way (avoids editing each early return): rename the current body to an inner closure-free helper and emit once. Concretely, replace the body after the `ToolCall` emit with: compute the `StepResult` into a local `result`, emit `ToolResult`, then return `result`. Restructure as follows — keep the existing logic but assign to `let result = { ... existing match producing StepResult ... };`, replacing each `return StepResult {...}` inside that block with the value (drop the `return`), and the trailing intent-mode `StepResult {...}` likewise. After the block:

```rust
    if let Some(cid) = opts.channel_id.as_deref() {
        let mut excerpt = result.output_text.clone();
        excerpt.truncate(2048);
        emit_channel(
            mur_home,
            cid,
            EventKind::ToolResult,
            serde_json::json!({
                "step_id": sid,
                "exit_code": result.exit_code,
                "success": result.success,
                "output": excerpt,
            }),
        );
    }
    result
```

> Implementation note: the existing `execute_step` uses early `return StepResult {…}` in the timeout/exec-error/command/intent branches. Convert the function to compute a single `let result: StepResult = { … }` (each former `return X` becomes the block's value via `return`-free `if/else`/`match` arms) so the `ToolResult` emit runs on every path. The retry path (collector) calls `execute_step` again, so each attempt naturally emits its own `ToolCall`+`ToolResult` pair — the desired per-attempt trail.

- [ ] **Step 5: Pass `mur_home` at both `execute_step` call sites**

In the per-rank spawn (`:388-405`), capture and pass `mur_home`. Add before the `for &i in &indices` loop:

```rust
        let opt_channel = opts.channel_id.clone();
        let home_owned = mur_home.to_path_buf();
```

Inside the spawn closure, add `channel_id: opt_channel.clone(),` to the `opts_clone` literal, capture `let home = home_owned.clone();` before `tokio::task::spawn`, and change the call to:

```rust
                execute_step(&step, &opts_clone, i, &home).await
```

In the retry call (`:499`), change to:

```rust
                            let retry_result = execute_step(step, opts, indices[ri], mur_home).await;
```

- [ ] **Step 6: Emit lifecycle `StateChange` from `execute_dag`**

After the fail-closed guard (Task 3) and the empty-graph early return (`:354-363`), add the start transition:

```rust
    if let Some(cid) = opts.channel_id.as_deref() {
        let _ = ChannelService::open(mur_home)
            .and_then(|svc| svc.transition(cid, ChannelState::Working, ChannelActor::System));
    }
```

Before each of the three `return Ok(PipelineOutput { … status: PipelineStatus::Failed … })` early returns (Abort `:470`, retry-exhausted `:507`) **and** the final `Ok(PipelineOutput { … })` (`:530`), emit the terminal state. Add a small closure near the top of `execute_dag` (after the start transition) to avoid repetition:

```rust
    let emit_final = |failed: bool| {
        if let Some(cid) = opts.channel_id.as_deref() {
            let st = if failed { ChannelState::Failed } else { ChannelState::Completed };
            let _ = ChannelService::open(mur_home)
                .and_then(|svc| svc.transition(cid, st, ChannelActor::System));
        }
    };
```

Call `emit_final(true);` immediately before the two `Failed` early-return blocks, and at the end (before the final `Ok(...)`) call `emit_final(overall_exit_code != 0);`.

> Note: `emit_final` borrows `opts` and `mur_home` immutably — both are `&` params live for the whole fn, so the closure compiles without ownership trouble. If the borrow checker objects inside the rank loop, inline the three calls instead.

- [ ] **Step 7: Run the test to verify it passes**

Run: `cargo test -p mur-core executor::dag::tests::channel_run_emits_attributed_event_trail`
Expected: PASS. Also run the existing executor tests to confirm no regression:
```bash
cargo test -p mur-core executor::dag::
```
Expected: all PASS (the non-channel tests pass `channel_id: None` via `DagExecOptions::default()`).

- [ ] **Step 8: Commit**

```bash
git add mur-core/src/executor/dag.rs
git commit -m "feat(executor): emit attributed ToolCall/ToolResult/StateChange over a channel

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 5: Wire `--channel` into `mur workflow run`

**Files:**
- Modify: `mur-core/src/cmd/workflow.rs`

- [ ] **Step 1: Find the flag plumbing**

The skill-based execute path is `workflow.rs:189-214`. Identify the enclosing fn's signature and how `yes`/`fail_fast` flags reach it (they are already parameters). Run:

```bash
grep -n "pub async fn\|yes: bool\|fail_fast\|DagExecOptions\|execute_dag\|Subcommand\|--channel\|channel" mur-core/src/cmd/workflow.rs
```

Note the function that owns the `execute_dag` call and the clap arg struct/enum for `workflow run` (likely in `mur-core/src/cli.rs` or a `WorkflowArgs`). Thread two new optional args from the CLI definition down to this function: `channel: Option<String>` and `channel_new: bool`.

- [ ] **Step 2: Resolve the channel id and set it on the options**

Replace the `DagExecOptions { … }` construction at `workflow.rs:191-196` with:

```rust
                            // v3a: optionally run the workflow OVER a channel.
                            // `--channel <id>` targets an existing channel;
                            // `--channel-new` creates one and prints its id.
                            let channel_id = if channel_new {
                                let mur_home = crate::paths::mur_root(None);
                                let svc = mur_channel::ChannelService::open(&mur_home)?;
                                let ch = svc.create_for_workflow(&matched.manifest.name)?;
                                eprintln!("▶ running over new channel {}", ch.id);
                                Some(ch.id)
                            } else {
                                channel.clone()
                            };
                            let opts = crate::executor::dag::DagExecOptions {
                                yes,
                                device_id: "cli".to_string(),
                                trigger: "manual",
                                channel_id,
                                ..Default::default()
                            };
```

- [ ] **Step 3: Add the clap flags**

In the `workflow run` arg definition (located in Step 1), add:

```rust
    /// Run the workflow over an existing Channel id (emits attributed events).
    #[arg(long)]
    channel: Option<String>,
    /// Create a fresh Channel for this run and print its id.
    #[arg(long = "channel-new", default_value_t = false)]
    channel_new: bool,
```

Thread both through the dispatch call to the function owning `execute_dag` (mirror how `yes` is threaded).

- [ ] **Step 4: Build to verify wiring**

Run: `cargo build -p mur-core`
Expected: compiles. (If `mur_channel` is not yet imported in `workflow.rs`, use the fully-qualified `mur_channel::ChannelService` as written, or add `use mur_channel::ChannelService;`.)

- [ ] **Step 5: Manual end-to-end check**

```bash
cargo run -p mur-core -- workflow run <some-category-workflow-skill> --channel-new
# → prints "▶ running over new channel <id>", runs the steps, then:
ls ~/.mur/channels/<id>/ && cat ~/.mur/channels/<id>/events.jsonl | head
# expect: StateChange(working) → ToolCall/ToolResult pairs → StateChange(completed)
# and in the Hub Work view (once v2 ships) the channel appears with a state badge.
```

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/workflow.rs   # plus the CLI arg-definition file from Step 1/3
git commit -m "feat(cli): mur workflow run --channel / --channel-new (run over a Channel)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 6: Quality gates + docs

**Files:**
- Modify: `CLAUDE.md` (CLI surface note), memory.

- [ ] **Step 1: Format**

```bash
cargo fmt
cargo fmt --check
```
Expected: clean. (No excluded-crate fmt needed — v3a touches only workspace members `mur-common`, `mur-channel`, `mur-core`, not the Tauri crates.)

- [ ] **Step 2: Clippy**

```bash
cargo clippy -p mur-common -p mur-channel -p mur-core -- -D warnings
```
Expected: no warnings.

- [ ] **Step 3: Full test run (nextest)**

```bash
cargo nextest run -p mur-common -p mur-channel -p mur-core
```
Expected: green. (Use `nextest`; plain `cargo test --workspace` spuriously fails ~7 `mur-core` tests per `mem:project_mur_core_flaky_tests`. The 4 pre-existing `conversations::summarize::rollup` LanceDB embedding-dim failures are unrelated to this work.)

- [ ] **Step 4: Docs**

- In `CLAUDE.md`, under the `mur` CLI surface, note `mur workflow run --channel <id>` / `--channel-new` runs a Workflow over a Channel (v3a). One line.
- No `~/.mur` data-model change (reuses v1 store; the `sig`/`key_version` fields are reserved and unset).

- [ ] **Step 5: Commit docs**

```bash
git add CLAUDE.md
git commit -m "docs: note mur workflow run --channel (v3a Workflow-over-Channel)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

- [ ] **Step 6: Update memory**

Record that v3a (Workflow-DAG mode 2 over a Channel) is implemented on branch `feat/unified-channel-v2` (or its own branch if split): executor emits attributed `StateChange`/`ToolCall`/`ToolResult` as `System`; approval-bearing workflows fail-closed; `ChannelEvent.sig`/`key_version` reserved for v3d. v3b (delegation) and v3c (HITL) still pending. Reference `mem:project_unified_channel_pr433`.

---

## Self-Review

**1. Spec coverage (against `2026-06-16-unified-channel-v3-design.md` §7 v3a):**
- "thread `ChannelService` + `channel_id` into the executor + per-rank spawn seam" → Task 3 (field) + Task 4 Step 5 (spawn threading). ✓
- "each DAG step emits an attributed channel event (ToolCall/ToolResult, actor System)" → Task 4 Steps 4-5. ✓
- "StateChange events for channel lifecycle (Working→Completed/Failed)" → Task 4 Step 6 + Task 2 `transition`. ✓
- "reserve the schema (`sig`/`key_version` + canonical sign-input doc-comment, both None)" → Task 1. ✓
- "needs_approval fail-closed; do NOT inherit non-TTY silent-skip" → Task 3 guard. ✓
- "no runtime changes, no new dispatcher method, no dial" → confirmed: only `mur-common`/`mur-channel`/`mur-core` change. ✓
- idempotency_key left None in v3a (dedup owned by v3c) → `append`/`transition` pass `None`. ✓

**2. Placeholder scan:** No "TBD"/"handle errors"/"similar to". Every code step has complete code. Two steps defer to a `grep` to locate the clap arg-definition file (Task 5 Step 1/3) because the `workflow run` arg struct's exact location is the one thing not yet read — the grep names exactly what to find and the steps give the exact code to add. Not a logic placeholder.

**3. Type consistency:**
- `ChannelEvent` gains `sig: Option<String>`, `key_version: Option<u32>` — used identically in channel.rs, store.rs, and the test literal.
- `ChannelService::{append, transition, create_for_workflow}` signatures are defined in Task 2 and consumed unchanged in Task 4 (`svc.append`, `svc.transition`) and Task 5 (`create_for_workflow`).
- `DagExecOptions.channel_id: Option<String>` defined in Task 3, read via `opts.channel_id.as_deref()` in Task 4 and set in Task 5. `execute_step`'s new `mur_home: &Path` param matches both call sites.
- `emit_channel(mur_home, channel_id, kind, payload)` and `state_str(ChannelState) -> &'static str` are defined once and called consistently.
- Actor is `ChannelActor::System` everywhere the executor writes (v3a sole-writer rule).

**4. Scope check:** Single sub-project (v3a), one branch's worth of work, ~6 tasks. No delegation, no HITL machinery, no runtime crate touched — those are v3b/v3c/v3d. Focused. ✓

No gaps found.
