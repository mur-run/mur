# Unified Channel v3b — Delegation / Mode 1 — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **Depends on v3a** (`2026-06-16-unified-channel-v3a-workflow-over-channel.md`): the channel-aware DAG executor seam (`DagExecOptions.channel_id`, channel-emitting `execute_step`, `ChannelService::{append, transition, create_for_workflow}`). v3a is **planned, not yet built** — if v3a's exact seam shifts during implementation, adjust the call sites referenced here. This plan extends that seam; it does not re-plumb it.

**Goal:** let a `category: Workflow` step **delegate** its sub-goal to a specialist MUR agent. The channel-aware executor dials the specialist over the existing A2A `message/send` streaming path (`DialMode::RequireRunning`) and records the work as **agent-attributed** events in the same channel: a `Delegation` event (actor `System`) then the specialist's reply as a `Message` event (actor `Agent{<canonical specialist name>}`), with the raw A2A task snapshot stored alongside for auditability.

**Architecture:** Decision A (concierge-mediated, single trusted writer) from the v3 spec. The executor (one OS process) remains the sole writer; the specialist stays a vanilla A2A agent that never learns the word "channel." Delegation is a new **executor step kind**, not a runtime change: a step with `delegate_to: Some(agent)` dials that agent instead of running `sh -c`. The reply is attributed to the dialed agent by name (provably the agent dialed, via `canonicalize_agent_name` + the target's `running.lock` socket). Deterministic `idempotency_key`s are **set** on the delegation events now (so they are present when v3c turns on dedup), but **no dedup is enforced in v3b** (that is v3c). HITL raised by the specialist is **recorded** as a mirror event in v3b; the interactive channel-gated relay is v3c.

**Tech Stack:** Rust — `mur-common` (`ProcedureStep`), `mur-channel` (`append_delegation`), `mur-core` (executor delegation branch, `a2a_dial`, `mur workflow` CLI). `mur-core` has `sha2` (for the idempotency key). **Zero** `mur-agent-runtime` / dispatcher / specialist changes.

**Scope guardrails (from `2026-06-16-unified-channel-v3-design.md` §7 v3b + §4):**
- Reply attribution uses the **canonical resolved** name (`canonicalize_agent_name`), never the user-typed string.
- `DialMode::RequireRunning` only — never `Auto` (Auto can cold-spawn an unintended process and corrupt attribution).
- On dial failure **nothing partial is attributed**: append a failure `Note`, return a failed `StepResult`, let the DAG `on_failure` drive retry.
- Set deterministic `idempotency_key` on Delegation + reply; **do not** add dedup logic (v3c owns `append_event` dedup).
- HITL from the specialist is recorded as a `HitlRequest` mirror event; resolution/relay is deferred to v3c.

**Key facts locked during exploration (do not re-derive):**
- `dial_message_streaming(home, agent_name, params, on_delta: FnMut(&str,bool,&str), on_hitl: FnMut(Value)) -> Result<Value>` (`a2a_dial.rs:179`). It canonicalizes internally, requires `running.lock`, sends `message/send`, streams `message/delta` (→ `on_delta`) and `tool/approval_needed` (→ `on_hitl`), and **returns the full task `Value`** (the `messages` array — this IS the "tasks/get snapshot").
- `canonicalize_agent_name(home, typed) -> String` (`a2a_dial.rs:45`).
- `ProcedureStep` (`manifest.rs:208`) derives `schemars::JsonSchema`; every field is `#[serde(default)]`. New fields must be `Option`/`Default` + JsonSchema-compatible.
- A2A reply shape: `task["messages"]` is an array of `{role, parts:[{kind:"text",text}]}`; the agent reply is the last `role=="agent"` message (mirrors `mur-hub-gui/src-tauri/src/chat.rs:162-171`).

---

## File Structure

**Modified:**
- `mur-common/src/skill/manifest.rs` — add `ProcedureStep.delegate_to: Option<String>`.
- `mur-channel/src/service.rs` — add `append_delegation(channel_id, target_agent, child_task_id, idempotency_key)` + a typed `DelegationPayload`.
- `mur-core/src/executor/dag.rs` — `DagExecOptions.run_id`; an `idempotency_key` helper; pure `build_delegate_params` + `extract_agent_reply`; the delegation branch in `execute_step`.
- `mur-core/src/cmd/workflow.rs` — generate a `run_id` per execution.

No new files. No `mur-agent-runtime` changes.

---

## Task 1: `ProcedureStep.delegate_to`

**Files:**
- Modify: `mur-common/src/skill/manifest.rs:208-262`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `manifest.rs` (find it with `grep -n "#\[cfg(test)\]" mur-common/src/skill/manifest.rs`):

```rust
    #[test]
    fn procedure_step_parses_delegate_to() {
        let yaml = "description: hand off to qa\ndelegate_to: qa\n";
        let s: ProcedureStep = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(s.delegate_to.as_deref(), Some("qa"));
        // Absent → None (every existing skill.yaml still parses).
        let s2: ProcedureStep = serde_yaml::from_str("description: local step\n").unwrap();
        assert_eq!(s2.delegate_to, None);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-common skill::manifest` (or the module path your grep reveals) — Expected: FAIL, no field `delegate_to`.

- [ ] **Step 3: Add the field**

In `ProcedureStep` (after `needs_approval`, `:261`):

```rust
    /// Delegate this step's sub-goal to a specialist MUR agent over A2A
    /// (v3b, Channel mode). When set, the channel-aware executor dials this
    /// agent via `message/send` instead of running `command`/`intent`, and
    /// attributes the reply to `Agent{<canonical agent name>}` in the channel.
    /// Ignored when the executor runs without a channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegate_to: Option<String>,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-common skill::manifest` — Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/skill/manifest.rs
git commit -m "feat(skill): ProcedureStep.delegate_to for channel delegation (v3b)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 2: `ChannelService::append_delegation` + typed payload

**Files:**
- Modify: `mur-channel/src/service.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `service.rs`:

```rust
    #[test]
    fn append_delegation_writes_typed_event() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("delegating-wf").unwrap();
        let ev = svc
            .append_delegation(&ch.id, "qa", "child-task-1", Some("idem-1".into()))
            .unwrap();
        assert_eq!(ev.kind, EventKind::Delegation);
        assert_eq!(ev.actor, ChannelActor::System);
        assert_eq!(ev.payload["target_agent"], "qa");
        assert_eq!(ev.payload["child_task_id"], "child-task-1");
        assert_eq!(ev.payload["parent_channel_id"], ch.id);
        assert_eq!(ev.idempotency_key.as_deref(), Some("idem-1"));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-channel service::tests::append_delegation_writes_typed_event` — Expected: FAIL, no method `append_delegation`.

- [ ] **Step 3: Implement it**

In `service.rs`, add a typed payload struct near the top (after the imports):

```rust
/// Typed payload for an [`EventKind::Delegation`] event. The concierge owns
/// `child_task_id` (the A2A task id it gave the dialed agent) and stamps the
/// canonical `target_agent` name.
#[derive(serde::Serialize)]
struct DelegationPayload<'a> {
    target_agent: &'a str,
    child_task_id: &'a str,
    parent_channel_id: &'a str,
}
```

Add inside `impl ChannelService` (after `append`):

```rust
    /// Append a `Delegation` event (actor `System`) recording that `target_agent`
    /// was handed the sub-goal under `child_task_id`. `idempotency_key` is set by
    /// the caller (deterministic in v3b) but NOT yet de-duplicated (v3c).
    pub fn append_delegation(
        &self,
        channel_id: &str,
        target_agent: &str,
        child_task_id: &str,
        idempotency_key: Option<String>,
    ) -> Result<ChannelEvent> {
        let payload = serde_json::to_value(DelegationPayload {
            target_agent,
            child_task_id,
            parent_channel_id: channel_id,
        })?;
        self.append(
            channel_id,
            ChannelActor::System,
            EventKind::Delegation,
            payload,
            idempotency_key,
        )
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-channel service::tests::append_delegation_writes_typed_event` — Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-channel/src/service.rs
git commit -m "feat(channel): ChannelService::append_delegation typed event (v3b)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 3: `run_id` + idempotency-key helper + pure delegation helpers

**Files:**
- Modify: `mur-core/src/executor/dag.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `dag.rs`:

```rust
    #[test]
    fn idem_key_is_deterministic_and_distinct() {
        let a = idem_key("chan", "run", "s0", "delegate");
        let b = idem_key("chan", "run", "s0", "delegate");
        let c = idem_key("chan", "run", "s0", "reply");
        assert_eq!(a, b, "same inputs → same key (crash-rerun stable)");
        assert_ne!(a, c, "different suffix → different key");
        assert_eq!(a.len(), 64, "sha256 hex");
    }

    #[test]
    fn build_delegate_params_threads_text_and_task_id() {
        let p = build_delegate_params("find the bug", "child-1");
        assert_eq!(p["task_id"], "child-1");
        assert_eq!(p["message"]["role"], "user");
        assert_eq!(p["message"]["parts"][0]["text"], "find the bug");
    }

    #[test]
    fn extract_agent_reply_takes_last_agent_message() {
        let task = serde_json::json!({
            "id": "t1",
            "messages": [
                {"role":"user","parts":[{"kind":"text","text":"q"}]},
                {"role":"agent","parts":[{"kind":"text","text":"partial "},{"kind":"text","text":"answer"}]}
            ]
        });
        assert_eq!(extract_agent_reply(&task), "partial answer");
        // No agent message → empty.
        let empty = serde_json::json!({ "messages": [{"role":"user","parts":[]}] });
        assert_eq!(extract_agent_reply(&empty), "");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-core executor::dag::tests::idem_key_is_deterministic_and_distinct` — Expected: FAIL, `idem_key`/`build_delegate_params`/`extract_agent_reply` not found.

- [ ] **Step 3: Add `run_id` to `DagExecOptions` + the helpers**

In `dag.rs`, add to `DagExecOptions` (after `channel_id` from v3a):

```rust
    /// Stable id for this logical run. Used to derive deterministic
    /// `idempotency_key`s for channel events. v3b sets keys; v3c enforces dedup,
    /// at which point a crash-rerun MUST reuse the same `run_id`. Empty = none.
    pub run_id: String,
```

Add `run_id: String::new(),` to the `Default` impl.

> Sequencing note: the v3 spec puts `run_id` plumbing in v3a. v3a sets no idempotency keys, so it deferred `run_id`; v3b adds it here as the first increment that needs it. (If v3a is implemented with `run_id` already present, skip this addition.)

Add the helper functions (near the top of `dag.rs`, after the imports — add `use sha2::{Digest, Sha256};`):

```rust
/// Deterministic idempotency key for a channel event: stable across a
/// crash-rerun of the same logical run, distinct per (channel, run, step, role).
fn idem_key(channel_id: &str, run_id: &str, step_id: &str, suffix: &str) -> String {
    let mut h = Sha256::new();
    h.update(format!("{channel_id}|{run_id}|{step_id}|{suffix}").as_bytes());
    format!("{:x}", h.finalize())
}

/// Build the `message/send` params for a delegated sub-goal.
fn build_delegate_params(text: &str, child_task_id: &str) -> serde_json::Value {
    serde_json::json!({
        "message": { "role": "user", "parts": [{ "kind": "text", "text": text }] },
        "task_id": child_task_id,
    })
}

/// Extract the specialist's reply: the last `role=="agent"` message's joined
/// text parts. Mirrors the Hub's `extract_text` over `task["messages"]`.
fn extract_agent_reply(task: &serde_json::Value) -> String {
    task.get("messages")
        .and_then(|m| m.as_array())
        .and_then(|msgs| {
            msgs.iter()
                .rev()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("agent"))
        })
        .and_then(|m| m.get("parts").and_then(|p| p.as_array()))
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}
```

Add `sha2` is already a `mur-core` dep (`Cargo.toml:59`), so no manifest change.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-core executor::dag::tests::idem_key_is_deterministic_and_distinct executor::dag::tests::build_delegate_params_threads_text_and_task_id executor::dag::tests::extract_agent_reply_takes_last_agent_message` — Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/executor/dag.rs
git commit -m "feat(executor): run_id + deterministic idem_key + delegation helpers (v3b)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 4: Delegation branch in `execute_step`

**Files:**
- Modify: `mur-core/src/executor/dag.rs`

> The dial itself needs a live agent, so it is verified by build + the manual E2E in Task 5 (network code is not unit-testable here — the same reason the v1 chat path has no dial unit test). The pure pieces it composes (`idem_key`, `build_delegate_params`, `extract_agent_reply`, `append_delegation`) are unit-tested in Tasks 2-3.

- [ ] **Step 1: Add the delegation branch**

In `execute_step` (`dag.rs:171`), **before** the `if let Some(cmd_template) = &step.command` command-mode block (`:211`), insert the delegation branch. It runs only when the step delegates AND a channel is attached:

```rust
    // ── Delegation (v3b): dial a specialist over A2A, attribute the reply ──
    if let (Some(target), Some(cid)) = (step.delegate_to.as_deref(), opts.channel_id.as_deref()) {
        let sid = step.id.clone().unwrap_or_else(|| step_index.to_string());
        let canonical = crate::a2a_dial::canonicalize_agent_name(mur_home, target);
        let child_task_id = format!("ct-{}", uuid::Uuid::now_v7());
        let deleg_key = idem_key(cid, &opts.run_id, &sid, "delegate");
        let reply_key = idem_key(cid, &opts.run_id, &sid, "reply");

        // Sub-goal text: explicit intent, else the step description.
        let goal_text = step.intent.clone().unwrap_or_else(|| step.description.clone());

        // Record the delegation up front (System actor, deterministic key).
        if let Ok(svc) = ChannelService::open(mur_home) {
            let _ = svc.append_delegation(cid, &canonical, &child_task_id, Some(deleg_key));
        }
        eprintln!("  Step {sid}: delegate → {canonical}: {goal_text}");

        let params = build_delegate_params(&goal_text, &child_task_id);
        // RequireRunning is enforced by dial_message_streaming (it bails if the
        // target has no running.lock). We surface a specialist HITL as a mirror
        // event only — interactive relay is v3c.
        let home_for_hitl = mur_home.to_path_buf();
        let cid_for_hitl = cid.to_string();
        let dial = crate::a2a_dial::dial_message_streaming(
            mur_home,
            &canonical,
            params,
            |_t, _thinking, _tid| { /* deltas buffered into the final snapshot */ },
            |hitl_params| {
                // Mirror the specialist's approval request into the channel for
                // visibility. v3c adds the interactive resolution path.
                if let Ok(svc) = ChannelService::open(&home_for_hitl) {
                    let _ = svc.append(
                        &cid_for_hitl,
                        ChannelActor::System,
                        EventKind::HitlRequest,
                        serde_json::json!({ "mirror": true, "from": "delegate", "params": hitl_params }),
                        None,
                    );
                }
            },
        );

        let result = match dial {
            Ok(task) => {
                let reply = extract_agent_reply(&task);
                // Attribute the reply to the dialed agent; store the raw task
                // snapshot alongside so attribution is auditable, not just asserted.
                if let Ok(svc) = ChannelService::open(mur_home) {
                    let _ = svc.append(
                        cid,
                        ChannelActor::Agent { id: canonical.clone() },
                        EventKind::Message,
                        serde_json::json!({
                            "text": reply,
                            "task_id": child_task_id,
                            "source_task": task,
                        }),
                        Some(reply_key),
                    );
                }
                let empty = reply.trim().is_empty();
                StepResult {
                    exit_code: if empty { 1 } else { 0 },
                    output_text: reply,
                    duration_ms: start.elapsed().as_millis() as u64,
                    failed_step: if empty { Some(step.description.clone()) } else { None },
                    success: !empty,
                }
            }
            Err(e) => {
                // Nothing partial is attributed; record a failure Note + fail the
                // step so the DAG's on_failure (Abort/Skip/Retry) decides.
                if let Ok(svc) = ChannelService::open(mur_home) {
                    let _ = svc.append(
                        cid,
                        ChannelActor::System,
                        EventKind::Note,
                        serde_json::json!({ "text": format!("delegate to {canonical} failed: {e:#}") }),
                        None,
                    );
                }
                eprintln!("  Step {sid}: delegate to {canonical} failed: {e:#}");
                StepResult {
                    exit_code: 1,
                    output_text: format!("delegate failed: {e}"),
                    duration_ms: start.elapsed().as_millis() as u64,
                    failed_step: Some(step.description.clone()),
                    success: false,
                }
            }
        };
        return result;
    }
```

> Note on the v3a `ToolCall`/`ToolResult` emit: in v3a (Task 4) `execute_step` emits a `ToolCall` at the top and a `ToolResult` at the bottom for the local-step path. For a **delegation** step the attributed `Delegation`+`Message` events above are the trail, so this branch `return`s before the v3a `ToolResult` emit. Confirm the v3a top-of-fn `ToolCall` emit is guarded to skip delegation steps (add `&& step.delegate_to.is_none()` to that v3a emit condition), so a delegated step does not get a spurious `System` `ToolCall`. This is the one cross-increment edit; make it when integrating onto the real v3a code.

- [ ] **Step 2: Build**

Run: `cargo build -p mur-core` — Expected: compiles. Run the existing executor tests to confirm no regression: `cargo test -p mur-core executor::dag::` — Expected: PASS (non-delegating steps unaffected; `delegate_to` defaults to `None`).

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/executor/dag.rs
git commit -m "feat(executor): delegation branch — dial specialist, attribute reply (v3b)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 5: `run_id` wiring + manual E2E + quality gates

**Files:**
- Modify: `mur-core/src/cmd/workflow.rs`

- [ ] **Step 1: Generate a `run_id` per execution**

In `workflow.rs`, where the `DagExecOptions` is built (the v3a `--channel` block), set a fresh `run_id`:

```rust
                            let opts = crate::executor::dag::DagExecOptions {
                                yes,
                                device_id: "cli".to_string(),
                                trigger: "manual",
                                channel_id,
                                run_id: format!("run-{}", uuid::Uuid::now_v7()),
                                ..Default::default()
                            };
```

- [ ] **Step 2: Build**

Run: `cargo build -p mur-core` — Expected: compiles.

- [ ] **Step 3: Manual end-to-end check (needs two running agents)**

Author a tiny `category: Workflow` skill with a delegating step, e.g. `~/.mur/skills/deleg-demo/skill.yaml`:

```yaml
name: deleg-demo
version: "0.1.0"
publisher: local
description: delegate a question to qa
category: workflow
content:
  abstract: demo
  procedure:
    variables: []
    steps:
      - description: ask qa to summarize
        intent: "Summarize what you can do in one sentence."
        delegate_to: qa
```

Then (with agent `qa` running — `mur agent run qa`):

```bash
cargo run -p mur-core -- workflow run deleg-demo --channel-new
id=$(ls -t ~/.mur/channels | head -1)
cat ~/.mur/channels/$id/events.jsonl
# expect, in order:
#   StateChange(working) [System]
#   Delegation {target_agent:"qa", child_task_id, parent_channel_id} [System]
#   Message {text:<qa reply>, source_task:{…}} [Agent{qa}]   ← agent-attributed
#   StateChange(completed) [System]
```

Confirm the reply event's `actor` is `{"kind":"agent","id":"qa"}` (canonical), and `idempotency_key` is set (a 64-char hex) on the Delegation + Message events.

- [ ] **Step 4: Quality gates**

```bash
cargo fmt && cargo fmt --check
cargo clippy -p mur-common -p mur-channel -p mur-core -- -D warnings
cargo nextest run -p mur-common -p mur-channel -p mur-core
```
Expected: clean + green (ignore the 4 pre-existing `conversations::summarize::rollup` failures; use `nextest` per `mem:project_mur_core_flaky_tests`).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/workflow.rs
git commit -m "feat(cli): generate run_id for channel workflow runs (v3b)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**1. Spec coverage (against `2026-06-16-unified-channel-v3-design.md` §7 v3b):**
- "append_delegation (typed payload, canonical target name)" → Task 2 + Task 4 (`canonicalize_agent_name`). ✓
- "dial via dial_message_streaming(DialMode::RequireRunning)" → Task 4 (RequireRunning enforced by the dial fn's `running.lock` check). ✓
- "on success append reply as Agent{canonical target} + store the tasks/get snapshot" → Task 4 (`Message` event with `source_task`). ✓
- "forward the specialist's on_hitl up / relay the human's tool/hitl_respond down" → Task 4 records a `HitlRequest` mirror; interactive relay explicitly deferred to v3c (consistent with the spec's trust-model invariant: channel HITL is a mirror until v3c/v3d). ✓ (scoped-down, flagged)
- "Sets a DETERMINISTIC idempotency_key on the Delegation+reply appends even before dedup is enforced" → Task 3 `idem_key` + Task 4 (`deleg_key`, `reply_key`). ✓
- "ZERO runtime/dispatcher/specialist changes" → confirmed: only `mur-common`/`mur-channel`/`mur-core`. ✓

**2. Placeholder scan:** No "TBD"/"handle errors"/"similar to". The dial branch is complete code; the only non-code deferral (interactive HITL relay) is an explicit v3c scope boundary, not a placeholder. Task 1 Step 1 and Task 5 Step 1 use a `grep`/locate for the test-module and the v3a `--channel` block — both name exactly what to find.

**3. Type consistency:**
- `ProcedureStep.delegate_to: Option<String>` (Task 1) read in Task 4.
- `ChannelService::append_delegation(channel_id, target_agent, child_task_id, idempotency_key: Option<String>)` (Task 2) called identically in Task 4.
- `DagExecOptions.run_id: String` (Task 3) read in Task 4 (`opts.run_id`), set in Task 5.
- `idem_key(channel_id, run_id, step_id, suffix) -> String`, `build_delegate_params(text, child_task_id)`, `extract_agent_reply(&Value) -> String` defined in Task 3, used in Task 4.
- Reply attribution actor is `ChannelActor::Agent { id: canonical }`; Delegation/Note/HitlRequest actor is `ChannelActor::System` — consistent with the v3a sole-writer rule and the v3b attribution upgrade.
- ⚠ Cross-increment edit (flagged in Task 4 Step 1 Note): guard the v3a top-of-`execute_step` `ToolCall` emit with `&& step.delegate_to.is_none()`.

**4. Scope check:** Single sub-project (v3b), built on v3a, ~5 tasks, three workspace crates, no runtime change. The dynamic "concierge LLM decomposes a goal into a plan" is NOT in v3b — v3b delivers the delegate-and-attribute mechanics for delegation steps; producing the plan from an LLM goal is a separate concern. Focused. ✓

No gaps found.
