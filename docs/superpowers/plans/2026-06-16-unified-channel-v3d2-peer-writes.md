# Unified Channel v3d-2 — A2 Peer-Writes-Own — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.
>
> The v3d-2 carve-out from the v3d signing plan. **Depends on v3d-1** (the `mur_channel::sign` primitives + `append_signed` + the channel-aware executor + v3b delegation), all present on this branch's base (`feat/unified-channel-v3d`).

**Goal:** a delegated specialist agent's **runtime signs and writes its own** channel events (its reply attributed to `Agent{self}`, signed by its own identity), instead of the concierge mediating and signing them as the router. Verify-on-fold then checks each event against **its actor's** pubkey — true multi-writer attribution. This unlocks the v3 spec's Decision-A "A2" destination.

**Architecture:** add a `channel/delegate` A2A method to `mur-agent-runtime`. When the concierge delegates a sub-goal, it dials `channel/delegate` on the specialist (passing the `channel_id`); the specialist's handler runs the turn (exactly like `message/send`) and then appends its **own** reply as a `Message` event with `actor = Agent{self}`, **signed with the specialist's `AgentIdentity`** (already loaded at runtime startup) via `ChannelService::append_signed`. The concierge stops appending the reply (it still appends the `Delegation` announcement as the router). Verification generalizes from "resolve the router pubkey" to "resolve each event's actor's pubkey" (`<mur_home>/agents/<actor_id>`), wired behind `MUR_CHANNEL_REQUIRE_SIG`.

**Tech Stack:** Rust — `mur-agent-runtime` (new dep + handler + dispatcher wiring), `mur-core` (executor delegation switch + per-actor verifier + gate). `mur-channel`'s signing core is unchanged.

**Scope guardrails:**
- v3d-2 signs the specialist's **reply `Message`** as `Agent{self}` (the headline attribution). Signing the specialist's own `ToolCall`/`ToolResult` events is a follow-on (the specialist's tool events aren't currently written to the channel at all — out of scope here).
- `DialMode::RequireRunning` for `channel/delegate` (the specialist must be live; never cold-spawn).
- Migration-safe: with `MUR_CHANNEL_REQUIRE_SIG` off, unsigned/legacy + mixed router/specialist signing all tolerated.
- The concierge still appends the `Delegation` announcement (router-signed) — only the **reply** moves to the specialist.

**Key facts (from exploration, this worktree):**
- Dispatcher: `mur-agent-runtime/src/supervisor.rs::build_dispatcher` (~:753) registers `MethodHandler`s (`protocol/a2a_server.rs:73` trait: `async fn handle(params, ctx) -> Result<Value, HandlerError>`). `message/send` handler template: `protocol/methods/message_send.rs` (parse `message`/`task_id`/`context.task_id` → `TaskSpec{input,context_task_id,task_id}` → `runner.run_sync(spec)` → return the `Task`).
- Runtime identity: `Arc<AgentIdentity>` loaded at `supervisor.rs:170`; profile (`profile.inner.name`, `profile.inner.identity.key_version`) loaded ~:140; `mur_home: PathBuf` ~:93. **`build_dispatcher` does NOT currently receive `identity`** — add it. `mur-agent-runtime/Cargo.toml` has **no `mur-channel` dep** — add it.
- Current delegation (`mur-core/src/executor/dag.rs` ~:413-534): appends `Delegation` via `channel_writer::append_as_writer(..., ROUTER_AGENT, System, …)`; dials `dial_message_streaming(message/send)`; appends the reply via `append_as_writer(..., ROUTER_AGENT, Agent{specialist}, Message, …)` (router-signed). `child_task_id`/`reply_key` (deterministic idem) computed there.
- `a2a_dial::dial_method(home, agent, method, params, DialMode)` (`a2a_dial.rs:68`).
- `mur_channel::sign::{verify_one, resolve_writer_pubkey, verify_log}`; `ChannelService::append_signed(channel_id, &AgentIdentity, kv, actor, kind, payload, idem)`. The v3d-1 gate (`mur-core/src/hitl/gate.rs`) resolves only `<mur_home>/agents/mur` — generalize per-actor.
- `mur-core::channel_writer::{append_as_writer, ROUTER_AGENT="mur"}`.

---

## File Structure

**Created:**
- `mur-agent-runtime/src/protocol/methods/channel_delegate.rs` — the `ChannelDelegateHandler`.
- `mur-core/src/channel_verify.rs` — per-actor verify-on-fold helpers.

**Modified:**
- `mur-agent-runtime/Cargo.toml` — add `mur-channel`.
- `mur-agent-runtime/src/protocol/methods/mod.rs` — `pub mod channel_delegate;`.
- `mur-agent-runtime/src/supervisor.rs` — thread `identity`/name/key_version/mur_home into `build_dispatcher`; register `channel/delegate`.
- `mur-core/src/executor/dag.rs` — dial `channel/delegate`; drop the router reply-append.
- `mur-core/src/hitl/gate.rs` — verify each candidate `HitlResponse` per-actor (via `channel_verify`).
- `mur-core/src/lib.rs` (+`main.rs`) — `pub mod channel_verify;`.

---

## Task 1: `mur-agent-runtime` gains a signed channel-append capability

**Files:**
- Modify: `mur-agent-runtime/Cargo.toml`; Create: `mur-agent-runtime/src/protocol/methods/channel_delegate.rs` (the append helper part first)

- [ ] **Step 1: Add the dep + a unit-tested append helper**

Add to `mur-agent-runtime/Cargo.toml [dependencies]`: `mur-channel = { path = "../mur-channel" }`.

Create `channel_delegate.rs` starting with a pure-ish append helper + its test:

```rust
//! `channel/delegate` (v3d-2): a delegated specialist runs the sub-goal and
//! appends its OWN reply, signed by its own identity, attributed to Agent{self}.

use std::path::Path;
use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, EventKind};
use mur_common::identity::AgentIdentity;

/// Append the specialist's reply to `channel_id` as `Agent{self}`, signed by the
/// specialist's identity (v3d-2 peer-writes-own). Best-effort: errors are
/// returned for the caller to log, never panic.
pub fn append_self_reply(
    mur_home: &Path, channel_id: &str, agent: &str, identity: &AgentIdentity, key_version: u32,
    reply_text: &str, task_id: &str, idem: Option<String>,
) -> anyhow::Result<()> {
    let svc = ChannelService::open(mur_home)?;
    svc.append_signed(
        channel_id, identity, key_version,
        ChannelActor::Agent { id: agent.to_string() },
        EventKind::Message,
        serde_json::json!({ "text": reply_text, "task_id": task_id }),
        idem,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn append_self_reply_is_signed_by_the_specialist() {
        let tmp = TempDir::new().unwrap();
        let id = AgentIdentity::generate();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("qa").unwrap();
        append_self_reply(tmp.path(), &ch.id, "qa", &id, 0, "the answer", "t-1", None).unwrap();
        let evs = svc.load_events(&ch.id).unwrap();
        let reply = evs.iter().rev().find(|e| e.kind == EventKind::Message
            && matches!(&e.actor, ChannelActor::Agent { id } if id == "qa")).unwrap();
        assert_eq!(reply.payload["text"], "the answer");
        // Signed by the specialist's key (not the router's).
        assert!(mur_channel::sign::verify_one(&ch.id, reply, &id.verifying_key_bytes(), true));
    }
}
```

- [ ] **Step 2: Run to verify it fails, then passes**

`cargo test -p mur-agent-runtime channel_delegate::tests::append_self_reply_is_signed_by_the_specialist` (declare `pub mod channel_delegate;` in `mur-agent-runtime/src/protocol/methods/mod.rs` so it compiles). Expect FAIL (no dep/module) → add dep + module → PASS.

- [ ] **Step 3: Commit**

```bash
git -C /Volumes/Firecuda4tb/Projects/mur-wt-v3d2 add mur-agent-runtime/Cargo.toml mur-agent-runtime/src/protocol/methods/channel_delegate.rs mur-agent-runtime/src/protocol/methods/mod.rs Cargo.lock
git -C /Volumes/Firecuda4tb/Projects/mur-wt-v3d2 commit -m "feat(runtime): mur-channel dep + signed self-reply append (v3d-2)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 2: `ChannelDelegateHandler` + dispatcher wiring

**Files:**
- Modify: `mur-agent-runtime/src/protocol/methods/channel_delegate.rs`, `mur-agent-runtime/src/supervisor.rs`

- [ ] **Step 1: Implement the handler** (mirrors `message_send.rs`, then calls `append_self_reply`)

Read `protocol/methods/message_send.rs` and `task_runner.rs` for the exact `TaskSpec`/`run_sync`/`TaskOutcome`/reply-extraction shapes, then add to `channel_delegate.rs`:

```rust
use std::sync::Arc;
use crate::protocol::a2a_server::{HandlerError, MethodHandler, RequestContext};
use crate::task_runner::{TaskRunner, TaskSpec};
use serde_json::Value;

pub struct ChannelDelegateHandler {
    runner: Arc<TaskRunner>,
    identity: Arc<AgentIdentity>,
    agent: String,
    key_version: u32,
    mur_home: std::path::PathBuf,
}

impl ChannelDelegateHandler {
    pub fn new(runner: Arc<TaskRunner>, identity: Arc<AgentIdentity>, agent: String, key_version: u32, mur_home: std::path::PathBuf) -> Self {
        Self { runner, identity, agent, key_version, mur_home }
    }
}

#[async_trait::async_trait]
impl MethodHandler for ChannelDelegateHandler {
    async fn handle(&self, params: Option<Value>, _ctx: &RequestContext) -> Result<Value, HandlerError> {
        let params = params.ok_or_else(|| HandlerError::invalid_params("missing params"))?;
        let channel_id = params.get("channel_id").and_then(Value::as_str)
            .ok_or_else(|| HandlerError::invalid_params("channel/delegate requires channel_id"))?
            .to_string();
        // Parse message + task ids EXACTLY as message_send does (reuse its helpers
        // if public; else mirror them). Build the TaskSpec and run the turn.
        let spec = TaskSpec { /* input: message, context_task_id, task_id — as message_send builds it */ };
        let outcome = self.runner.run_sync(spec).await
            .map_err(|e| HandlerError::internal(format!("delegate run failed: {e}")))?;
        let task = /* extract the Task from the outcome, as message_send does */;
        let reply = /* extract the agent reply text from task.messages (last agent msg), as message_send / chat.rs does */;
        let task_id = task.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
        // Peer-writes-own: append the reply signed by THIS agent.
        if let Err(e) = super::channel_delegate::append_self_reply(
            &self.mur_home, &channel_id, &self.agent, &self.identity, self.key_version,
            &reply, &task_id, params.get("idempotency_key").and_then(Value::as_str).map(str::to_string),
        ) {
            tracing::warn!("channel/delegate self-append failed: {e:#}");
        }
        Ok(task)
    }
}
```

> Fill the `/* … */` parts by reading `message_send.rs` — reuse its param-parsing + Task/reply extraction. If those helpers are private, mirror them minimally. The KEY new behaviour is the `append_self_reply` call. Keep streaming optional (not required for v3d-2; a non-streaming `run_sync` is fine).

- [ ] **Step 2: Thread identity into `build_dispatcher` + register**

In `supervisor.rs`: add `identity: &Arc<AgentIdentity>` (and the agent name + `profile.inner.identity.key_version`) to `build_dispatcher`'s parameters (update its call site, which already has `identity` in scope from `:170` and `mur_home`). Register:

```rust
    d.register("channel/delegate", Box::new(ChannelDelegateHandler::new(
        runner.clone(), identity.clone(), profile_name.clone(), key_version, mur_home.to_path_buf(),
    )));
```

- [ ] **Step 3: Build + commit**

`cargo build -p mur-agent-runtime`. Then commit (`feat(runtime): channel/delegate handler — specialist writes its own signed reply (v3d-2)`).

## Task 3: Concierge dials `channel/delegate` (stop router-appending the reply)

**Files:**
- Modify: `mur-core/src/executor/dag.rs`

- [ ] **Step 1: Switch the delegation dial + drop the reply-append**

In the `dag.rs` delegation branch: keep the `Delegation` announcement (`append_as_writer(..., ROUTER_AGENT, System, Delegation, …)`). Replace the `dial_message_streaming(message/send)` + the subsequent `append_as_writer(..., ROUTER_AGENT, Agent{specialist}, Message, …)` with:
- build params `{ message, channel_id: cid, task_id: child_task_id, idempotency_key: reply_key }`;
- `let task = a2a_dial::dial_method(mur_home, &canonical, "channel/delegate", params, DialMode::RequireRunning)?;`
- extract the reply text from the returned `task` (for the step `StepResult.output_text` only — do NOT append it; the specialist already did).
- On dial error, keep the existing failure `Note` + failed `StepResult`.

> The deterministic `reply_key` is now passed to the specialist as `idempotency_key` so a resume/retry dedups on the SAME key the specialist writes under. The `child_task_id` is the delegated turn id.

- [ ] **Step 2: Build + adjust the delegation test**

`cargo build -p mur-core`. The existing v3b delegation test asserted the concierge appended an `Agent{specialist}` reply; under v3d-2 the specialist writes it (which a unit test without a running specialist can't exercise). Update/relax that test to assert the `Delegation` event is appended and the dial is attempted (or mark it `#[ignore]` with a note that the reply-write is now the specialist's, covered by Task 1's `append_self_reply` test + the manual E2E). Document the change.

- [ ] **Step 3: Commit** (`feat(executor): delegate via channel/delegate; specialist writes its reply (v3d-2)`).

## Task 4: Per-actor verify-on-fold

**Files:**
- Create: `mur-core/src/channel_verify.rs`; Modify: `mur-core/src/hitl/gate.rs`, `mur-core/src/lib.rs` (+`main.rs`)

- [ ] **Step 1: Write the failing test + helper**

Create `mur-core/src/channel_verify.rs`:

```rust
//! Per-actor verify-on-fold (v3d-2): each event is verified against ITS actor's
//! pubkey (`<mur_home>/agents/<id>`), not a single channel writer.
use std::path::Path;
use mur_common::channel::{ChannelActor, ChannelEvent};

/// Resolve the pubkey that should have signed `actor`'s events. Agent{id} →
/// that agent's home; System/Human → the router ("mur") which writes them.
pub fn actor_pubkey(mur_home: &Path, actor: &ChannelActor, key_version: Option<u32>) -> Option<[u8; 32]> {
    let agent = match actor {
        ChannelActor::Agent { id } => id.as_str(),
        _ => crate::channel_writer::ROUTER_AGENT, // System/Human are router-written
    };
    mur_channel::sign::resolve_writer_pubkey(&mur_home.join("agents").join(agent), key_version)
}

/// True if `ev` verifies against its actor's key (present sig must verify;
/// missing sig tolerated iff `!require_sig`).
pub fn verify_event(mur_home: &Path, channel_id: &str, ev: &ChannelEvent, require_sig: bool) -> bool {
    match actor_pubkey(mur_home, &ev.actor, ev.key_version) {
        Some(pk) => mur_channel::sign::verify_one(channel_id, ev, &pk, require_sig),
        None => !require_sig, // no key on disk → tolerate unless enforcing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::channel::EventKind;
    use mur_common::identity::AgentIdentity;

    #[test]
    fn event_verifies_against_its_own_actor_key() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Specialist "qa" with its own identity on disk.
        let qa_home = tmp.path().join("agents").join("qa");
        std::fs::create_dir_all(&qa_home).unwrap();
        let qa = AgentIdentity::generate(); qa.save(&qa_home).unwrap();
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("qa").unwrap();
        svc.append_signed(&ch.id, &qa, 0, ChannelActor::Agent { id: "qa".into() },
            EventKind::Message, serde_json::json!({"text":"hi"}), None).unwrap();
        let ev = svc.load_events(&ch.id).unwrap().pop().unwrap();
        assert!(verify_event(tmp.path(), &ch.id, &ev, true), "qa-signed event verifies vs qa's key");
        // A forged event (signed by a different key, attributed to qa) fails.
        let imposter = AgentIdentity::generate();
        let forged_sig = mur_channel::sign::sign_event(&imposter, &ch.id, &ev.actor, ev.kind, &ev.payload, None);
        let mut forged = ev.clone(); forged.sig = Some(forged_sig);
        assert!(!verify_event(tmp.path(), &ch.id, &forged, true), "imposter sig rejected");
    }
}
```

- [ ] **Step 2: Run (fail → pass).** Declare `pub mod channel_verify;` in `lib.rs` (and `main.rs`). `cargo test -p mur-core channel_verify::`.

- [ ] **Step 3: Use it in the gate.** In `mur-core/src/hitl/gate.rs`, replace the router-only `resolve_writer_pubkey(...mur...)` + `verify_one` with `crate::channel_verify::verify_event(mur_home, channel_id, resp, require_sig)` so a `HitlResponse` is verified against whoever's actor wrote it (still the router today, but now correct if a delegate ever responds). Keep the fail-closed "ignore unverifiable response" behaviour + the `require_sig` parameter from v3d-1.

- [ ] **Step 4: Build + test + commit** (`cargo nextest run -p mur-channel -p mur-core`; `feat(hitl): per-actor verify-on-fold; gate verifies response by its author (v3d-2)`).

## Task 5: Quality gates + docs

- [ ] **Step 1:** `cargo fmt && cargo fmt --check`; `cargo clippy -p mur-agent-runtime -p mur-channel -- -D warnings` (and confirm v3d-2-touched mur-core files are clippy-clean; ignore pre-existing debt in untouched files); `cargo nextest run -p mur-common -p mur-channel -p mur-core -p mur-agent-runtime` (green bar the 4 pre-existing `conversations::summarize::rollup` failures).
- [ ] **Step 2: Manual E2E** (two running agents): a workflow with a `delegate_to: qa` step over a channel → the channel shows a `Delegation` (router-signed) then a `Message` authored by `Agent{qa}` **signed by qa's key** (verify with `MUR_CHANNEL_REQUIRE_SIG=1` that it's accepted, and that a hand-forged qa line is dropped).
- [ ] **Step 3: Docs + memory.** `CLAUDE.md`: note `channel/delegate` (v3d-2) — delegated specialists write+sign their own channel events; verify-on-fold is per-actor. Memory: v3d-2 done; A2 peer-writes-own live; remaining follow-on = signing specialists' own tool events + wiring `verify_log` into `load_events` generally.
- [ ] **Step 4: Commit** docs.

---

## Self-Review

**1. Spec coverage (`2026-06-16-unified-channel-v3-design.md` §7 v3d + the v3d-1 carve-out):**
- "runtime `mur-channel` dep + `channel/delegate` dispatcher method + specialists sign their own events" → Tasks 1-2. ✓
- "the specialist's run posts attributed events back into the SAME stream" → Task 2 (`append_self_reply`, `Agent{self}`-signed) + Task 3 (concierge stops mediating the reply). ✓
- "verify-on-fold per-actor" → Task 4 (`channel_verify::verify_event`) + gate uses it. ✓
- Migration-safe (mixed router/specialist signing tolerated when not enforcing) → `require_sig` threading retained. ✓

**2. Placeholder scan:** The handler (Task 2) has `/* … */` slots that the implementer fills by reading `message_send.rs` (param/Task/reply extraction) — these are "reuse the existing template" pointers, not invented logic; the NEW behaviour (`append_self_reply`) is complete. Task 3's test relaxation is explicit. Everything else is complete code.

**3. Type consistency:** `append_self_reply(mur_home, channel_id, agent, identity, key_version, reply_text, task_id, idem)` (Task 1) called by the handler (Task 2). `actor_pubkey`/`verify_event` (Task 4) reuse `mur_channel::sign::{resolve_writer_pubkey, verify_one}` and `channel_writer::ROUTER_AGENT`. `dial_method(..., "channel/delegate", params, RequireRunning)` (Task 3) matches the registered method name (Task 2). `idempotency_key`/`reply_key` flows concierge→specialist so dedup keys align.

**4. Scope check:** v3d-2 = peer-writes-own reply + per-actor verify, across `mur-agent-runtime` (new) + `mur-core` (executor/gate). The runtime handler's turn-execution is integration (needs an LLM); the append + per-actor verify cores are unit-tested. Specialist tool-event signing + general read-path `verify_log` wiring are named follow-ons. Focused. ✓
