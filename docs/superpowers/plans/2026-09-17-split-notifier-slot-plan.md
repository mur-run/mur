# Splitting the notifier slot Implementation Plan
> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give approval authority its own map so borrowing it no longer silences the turn's audience — a CLI-spawn turn's `step/started` and `step/completed` reach the client watching it instead of the shim, which drops them.

**Architecture:** `client_notifiers` currently answers two questions with one entry. Three readers use it and they split two-to-one: step events and `message/delta` want *who is watching*; `gate_response` wants *who can answer*. Adding `approval_sinks` lets `tools/call` borrow the second without disturbing the first.

**Tech stack:** Rust (edition 2024, `mur-agent-runtime`).

### Global Constraints

- **No second execution path.** The tripwire stays green.
- Fail closed. A task with no approval sink still denies; splitting the map must not turn "nobody can answer" into "anybody can".
- An in-process turn's behaviour does not change. Its client is both audience and approver, so both maps hold the same sender.

### What the split is, in one table

| reader | map after this change | means |
|---|---|---|
| `guarded.rs` `step_notifier` | `client_notifiers` | who is watching |
| `task_runner.rs` `emit_live_warning` | `client_notifiers` | who is watching |
| `guarded.rs` `gate_response` | **`approval_sinks`** | who can answer |

Fifteen touch points, but only three are reads. The rest are writers and
tests, and the writers are where the care goes: a register that forgets one
map is a turn nobody can approve, or one whose approvals go to a stale
connection.

## File structure

| File | Status | Responsibility |
|---|---|---|
| `mur-agent-runtime/src/task_runner.rs` | modified | The second map, and the writers that keep the two in step. |
| `mur-agent-runtime/src/tools/guarded.rs` | modified | `gate_response` reads the approval map; `step_notifier` keeps the audience one. |
| `mur-agent-runtime/src/protocol/methods/tools.rs` | modified | Borrow the approval map only. |

---

## Task 1 — The second map, written in step with the first

### Interfaces

**Produces:**
```rust
// TaskRunner
approval_sinks: Arc<tokio::sync::Mutex<HashMap<String, ApprovalSink>>>
// register_client_notifier / unregister_client_notifier now write both.
// borrow_client_notifier / restore_client_notifier touch approval_sinks only.
```

### Steps

- [ ] Add the field beside `client_notifiers` in
      `mur-agent-runtime/src/task_runner.rs`:

```rust
    /// Who may answer an approval for a task, as distinct from who is
    /// watching it.
    ///
    /// For an in-process turn these are the same connection and both maps
    /// hold it. They diverge for a CLI-spawn turn: the shim can answer,
    /// because it can ask the CLI's user through `elicitation/create`, but
    /// the audience is whoever ran `mur agent send`. One map served both
    /// until the shim borrowed it and the turn's client went silent.
    approval_sinks: Arc<tokio::sync::Mutex<HashMap<String, ApprovalSink>>>,
```

- [ ] Initialise it next to `client_notifiers` in the constructor:

```rust
            approval_sinks: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
```

- [ ] `register_client_notifier` writes both — the attached client is both
      audience and approver. Add, after its existing insert:

```rust
        self.approval_sinks
            .lock()
            .await
            .insert(task_id.to_string(), (tx, can_approve));
```

      The existing insert consumes `tx`, so clone it for the first and move it
      into the second; if the compiler objects, clone for both rather than
      reordering the existing line.

- [ ] `unregister_client_notifier` clears both. Add beside its existing
      remove:

```rust
        self.approval_sinks.lock().await.remove(task_id);
```

- [ ] The "nobody can answer" helper (the one inserting a dropped-receiver
      sender with `can_approve = false`) moves to `approval_sinks` **only**.
      Declaring that nobody can approve must not also declare that nobody is
      watching — those became different statements with this change.

- [ ] `borrow_client_notifier` and `restore_client_notifier` change their map
      from `client_notifiers` to `approval_sinks`, and nothing else about
      them changes. Rename them to `borrow_approval_sink` /
      `restore_approval_sink` so the call site cannot read as if it were
      taking the audience.

- [ ] Build:

```bash
cargo build -p mur-agent-runtime
```

- [ ] Commit: `git add -A && git commit -m "feat(runtime): approval authority gets its own map"`

---

## Task 2 — Point each reader at the map it meant

### Steps

- [ ] `GuardedToolCall` needs both. Add the field beside `client_notifiers`
      in `mur-agent-runtime/src/tools/guarded.rs`:

```rust
    pub(crate) approval_sinks:
        Arc<tokio::sync::Mutex<HashMap<String, crate::task_runner::ApprovalSink>>>,
```

      and clone it through in `TaskRunner::guarded()` beside the existing
      `client_notifiers` line.

- [ ] In `gate_response`, change the lookup from `client_notifiers` to
      `approval_sinks`. That is the one line; `step_notifier` below it stays
      on `client_notifiers`, which is the whole point of the change.

- [ ] `tools.rs`: rename the two calls to match Task 1's new names. No other
      change — it was already borrowing exactly the right thing, just from
      the wrong map.

- [ ] Run the suites and watch them pass:

```bash
cargo test -p mur-agent-runtime protocol::methods::tools
cargo test -p mur-agent-runtime hitl
```

Expected: `protocol::methods::tools` 9 passed; `hitl` unchanged from `main`.

- [ ] Commit: `git add -A && git commit -m "fix(runtime): step events follow the audience, approvals follow the approver"`

---

## Task 3 — Prove the thing that was broken

### Steps

- [ ] Add this test inside the existing `mod tests` in
      `mur-agent-runtime/src/protocol/methods/tools.rs`:

```rust
    #[tokio::test]
    async fn step_events_reach_the_watcher_while_the_approver_is_borrowed() {
        // The defect this change exists for. A CLI-spawn turn hands the shim
        // the turn's own task id, so `tools/call` arrives on a task the
        // user's client is already watching. Before the split, the borrow
        // displaced that client and every step event went to the shim, which
        // drops what it does not recognise.
        // `Allow` is load-bearing: with no policy a tool defaults to `Ask`,
        // and the gate answers before any step event is emitted — the test
        // would then be measuring the HITL path, not step routing.
        use mur_common::agent::{ToolPolicy, ToolRule};
        let runner = Arc::new(
            runner_with_probe()
                .with_tools_policy(vec![ToolRule {
                    pattern: "*".into(),
                    policy: ToolPolicy::Allow,
                    risk: None,
                }])
                .with_pending_approvals(Default::default()),
        );
        let (user_tx, mut user_rx) = tokio::sync::mpsc::channel::<Value>(16);
        runner.register_client_notifier("t-1", user_tx, true).await;

        let (shim_tx, _shim_rx) = tokio::sync::mpsc::channel::<Value>(16);
        let ctx = RequestContext {
            notifier: Some(shim_tx),
        };
        let _ = ToolsCallHandler::new(runner.clone())
            .handle(
                Some(json!({ "task_id": "t-1", "name": "probe_tool" })),
                &ctx,
            )
            .await
            .expect("handler");

        // The watcher saw the tool run.
        let mut methods = Vec::new();
        while let Ok(Some(n)) =
            tokio::time::timeout(std::time::Duration::from_millis(200), user_rx.recv()).await
        {
            if let Some(m) = n["method"].as_str() {
                methods.push(m.to_string());
            }
        }
        assert!(
            methods.iter().any(|m| m == "step/started"),
            "the watching client saw no step/started: {methods:?}"
        );
        assert!(
            methods.iter().any(|m| m == "step/completed"),
            "the watching client saw no step/completed: {methods:?}"
        );
    }
```

- [ ] Run it and watch it pass:

```bash
cargo test -p mur-agent-runtime step_events_reach_the_watcher
```

Expected: `test result: ok. 1 passed; 0 failed`.

- [ ] **Prove it can fail** — and falsify the *borrow target*, not
      `gate_response`. Point `borrow_approval_sink` and
      `restore_approval_sink` back at `client_notifiers` and run it again.
      Expected: `the watching client saw no step/started: []`.

      Reverting `gate_response`'s lookup instead does **not** turn it red,
      and that is worth understanding rather than working around: the test
      uses an `Allow` policy, so `gate_response` returns before it reads any
      map. What this test measures is the borrow displacing the audience,
      which is the defect. Put the two functions back and watch it pass.

- [ ] Confirm the hard constraint and the earlier regression both hold:

```bash
cargo test -p mur-agent-runtime execute_is_called_from_guarded_only
cargo test -p mur-agent-runtime tools_call_does_not_strand
```

- [ ] Lint and format with CI's own invocation:

```bash
cargo clippy --all --all-targets --no-deps -- -D warnings && cargo fmt --check
```

- [ ] Commit: `git add -A && git commit -m "test(a2a): a borrowed approver does not silence the watcher"`

## Done when

- [ ] `cargo test -p mur-agent-runtime protocol::methods::tools` — 10 passed.
- [ ] The falsification in Task 3 turned the new test red, and putting the
      line back turned it green.
- [ ] `execute_is_called_from_guarded_only` passes.
- [ ] `cargo test -p mur-agent-runtime` — no non-`llm::` failure that was not
      failing before.
- [ ] `cargo clippy --all --all-targets --no-deps -- -D warnings && cargo fmt --check` — clean.

## Not in this plan

- **The shim forwarding anything.** It stays a transport. Step events now
  never reach it, which is the repair — there is nothing for it to forward.
- **`message/delta` for a spawned turn.** The CLI's own output is not
  streamed back at all yet (`run_turn` collects and returns); that is a
  separate change and a separate decision about what a spawned turn's
  narration should look like.
