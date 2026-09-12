# Plan: fail at dispatch, not after the budget — preflight and non-retryable authorization

> Execute with **`mur-executing-plans`**. Spec:
> `docs/superpowers/specs/2026-09-12-execution-limits-design.md` §3.8, D9, §7; §9 step 6.
> Base: `main` after #1279 (step 5).

**Goal.** A task that needs a tool the agent does not have fails before its first model call, naming the tool and the command that grants it; an authorization refusal is told to the model once and the tool leaves its list for the rest of the turn, so nothing spins on `not authorized` ×3 again.

**Architecture.** One error class, `ToolError::NotAuthorized`, raised where authorization is decided today (`fleet_run` allowlist, policy `Deny`, an MCP server answering `not authorized: …` — `parallel_jobs.targets` is the known one). The loop keeps a per-turn set of tools that answered that way and stops offering them. Preflight is data, not inference: a fleet declares what its work needs (`fleet.yaml needs: [write_file, edit_file, bash]`), the DAG hands that list to every delegate as the A2A parameter `needs`, and `channel/delegate` compares it against the runtime's own tool inventory + policy before building a prompt. No new source of truth: the runtime checks the same `ToolRule` list and the same registered tools the gate uses (D9).

**Tech stack.** Rust 2024, `cargo nextest`. `mur-core` env: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432`; from a worktree add `CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target`.

## Global Constraints (from the spec)

- D9: authorization gates are **untouched** — `fleet_run` allowlist, `parallel_jobs.targets`, tool Ask/Deny, path grants. Nothing here grants; two behaviours change: a missing capability fails **at dispatch**, and authorization errors are **non-retryable**.
- §3.8: the preflight message is `cannot start: <agent> has no <tool> — mur agent perm tool-allow <agent> <tool>`; the check reads the same `ToolRule` list the gate reads. `ToolError::NotAuthorized` is terminal for that tool for the rest of the turn: the model is told once and the tool is removed from its list.
- §7: a coding brief on an agent with `write_file` denied fails before the first LLM call.
- "Needs" are declared, never guessed from prose. A fleet with no `needs:` gets no preflight — the old behaviour, exactly.
- A new field on `mur_common::fleet::Fleet` must be `#[serde(default)]`; grep `mur-hub-gui/src-tauri/src` for `Fleet {` literals and for every `pub fn` whose signature changes; Hub `cargo check` last.
- Before every commit: `cargo fmt`, `cargo clippy -p <crate> --all-targets -- -D warnings` (exit code), the named tests green.

## File structure

| File | Responsibility | Task |
|---|---|---|
| `mur-common/src/authz.rs` (new), `lib.rs` | `NOT_AUTHORIZED_PREFIX`, `is_not_authorized(&str)`, `not_authorized(msg) -> String`; tests | 1 |
| `mur-agent-runtime/src/tools/mod.rs` | `ToolError::NotAuthorized(String)` | 1 |
| `mur-agent-runtime/src/tools/fleet_run.rs` | allowlist miss → `NotAuthorized` | 1 |
| `mur-agent-runtime/src/tools/mcp.rs` | an `isError` result whose text is a not-authorized message → `NotAuthorized` | 1 |
| `mur-core/src/executor/jobs.rs` | `check_authorization` message uses the shared prefix | 1 |
| `mur-agent-runtime/src/task_runner.rs` | per-turn `disabled_tools`; `NotAuthorized` and policy `Deny` disable the tool; `tool_defs` filtered per iteration; `missing_tools(&[String])`; tests | 2, 3 |
| `mur-common/src/fleet.rs` | `Fleet.needs: Vec<String>` | 3 |
| `mur-core/src/executor/dag.rs` | `DagExecOptions.needs`, `channel/delegate` params carry `needs`; test | 3 |
| `mur-core/src/cmd/fleet/loop_run.rs`, `run.rs` | pass `fleet.needs` | 3 |
| `mur-core/src/cmd/fleet/show.rs` | print `needs` | 3 |
| `mur-agent-runtime/src/protocol/methods/channel_delegate.rs`, `message_send.rs` | parse `needs`; preflight before the turn; test | 3 |
| `CLAUDE.md` | one line | 4 |

---

## Task 1 — one error class for "you may not"

**Interfaces.**
- Produces:

```rust
// mur_common::authz
pub const NOT_AUTHORIZED_PREFIX: &str = "not authorized:";
pub fn not_authorized(msg: &str) -> String            // "not authorized: {msg}"
pub fn is_not_authorized(text: &str) -> bool           // trims, strips a leading "Error: ", case-insensitive prefix match
// mur_agent_runtime::tools
pub enum ToolError { Execution(String), Unknown(String), InvalidInput(String), NotAuthorized(String) }
```

### Steps

- [x] **1.1 Write the failing tests** — new file `mur-common/src/authz.rs`:

```rust
//! The one spelling of "you may not" that every authorization gate uses
//! (spec 2026-09-12 execution-limits §3.8, D9). The gates stay where they
//! are; this is how their refusals are recognised across a process boundary
//! — an MCP server's `isError` text, a tool's error — so the loop can treat
//! them as terminal for the tool instead of as something to retry.

pub const NOT_AUTHORIZED_PREFIX: &str = "not authorized:";

/// `not authorized: <msg>` — the message every gate emits.
pub fn not_authorized(msg: &str) -> String {
    format!("{NOT_AUTHORIZED_PREFIX} {msg}")
}

/// Does this text (possibly wrapped by an MCP server as `Error: …`) carry a
/// refusal? Prefix only — a tool that merely mentions authorization in its
/// output is not refusing.
pub fn is_not_authorized(text: &str) -> bool {
    let t = text.trim_start();
    let t = t.strip_prefix("Error:").map(str::trim_start).unwrap_or(t);
    t.len() >= NOT_AUTHORIZED_PREFIX.len()
        && t[..NOT_AUTHORIZED_PREFIX.len()].eq_ignore_ascii_case(NOT_AUTHORIZED_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_are_recognised_with_or_without_the_mcp_wrapper() {
        assert!(is_not_authorized(&not_authorized("target 'ghost' for parallel_jobs")));
        assert!(is_not_authorized("Error: not authorized: fleet_run denied"));
        assert!(is_not_authorized("  NOT AUTHORIZED: x"));
        assert!(!is_not_authorized("the file says: not authorized: nope"), "prefix, not substring");
        assert!(!is_not_authorized("permission denied"));
    }
}
```

  Register `pub mod authz;` in `mur-common/src/lib.rs` (alphabetical, after `agent_name`).

- [x] **1.2 Watch it pass** — `cargo nextest run -p mur-common --lib -E 'test(/authz::/)'`.

- [x] **1.3 The variant and its three sources** —

  `mur-agent-runtime/src/tools/mod.rs`:

```rust
    /// An authorization gate said no — the fleet_run allowlist, a tool
    /// policy `Deny`, an MCP server's refusal. Terminal for the tool for the
    /// rest of the turn (spec §3.8): the model is told once, then the tool
    /// leaves its list. Never retried.
    #[error("{0}")]
    NotAuthorized(String),
```

  `fleet_run.rs`: the allowlist miss (`"fleet_run denied: agent … not authorized …"`) becomes `Err(ToolError::NotAuthorized(mur_common::authz::not_authorized(&format!("fleet_run: agent '{}' / fleet '{fleet}' — the user must add them to `fleet_run.agents` / `fleet_run.fleets` in ~/.mur/config.yaml (deny-by-default)", self.agent_name))))`; update `denies_unauthorized_agent` to `assert!(matches!(err, ToolError::NotAuthorized(_)))`.

  `mcp.rs` `execute`: replace `if is_error { Err(ToolError::Execution(text)) }` with

```rust
        if is_error {
            // A refusal is not a failure to retry: the server said "may not".
            return Err(if mur_common::authz::is_not_authorized(&text) {
                ToolError::NotAuthorized(text)
            } else {
                ToolError::Execution(text)
            });
        }
```

  `mur-core/src/executor/jobs.rs` `check_authorization`: the `bail!` message becomes `mur_common::authz::not_authorized(&format!("target '{}' for parallel_jobs (deny-by-default) — add it under `parallel_jobs.targets` in {}, e.g.\n\nparallel_jobs:\n  targets:\n    - {}", …))` (the existing tests assert `contains("not authorized")` — still true).

- [x] **1.4 Watch it pass** — `cargo nextest run -p mur-agent-runtime --lib -E 'test(/fleet_run::|mcp::/)'`, `cargo nextest run -p mur-core --lib -E 'test(/executor::jobs::/)'`, `-p mur-mcp-server`. Any `match` over `ToolError` the compiler names gets a `NotAuthorized(m) =>` arm that behaves like `Execution` for now (Task 2 changes the loop's).

- [x] **1.5 fmt + clippy on `mur-common`, `mur-agent-runtime`, `mur-core`, `mur-mcp-server`**, then **commit**:

```
feat(authz): ToolError::NotAuthorized — one spelling for every gate's refusal

fleet_run's allowlist miss, an MCP server's `not authorized:` result and
parallel_jobs.targets now share one prefix and one error variant, so the
loop can tell "may not" from "failed". No gate changes what it allows.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 2 — told once, then gone for the turn

**Interfaces.**
- Consumes: `ToolError::NotAuthorized` (Task 1), `handle_tool_call`, `gate_response`'s `ToolPolicy::Deny` arm, the `tool_defs` list in `run_agentic_loop`.
- Produces: in `run_agentic_loop` a `disabled: HashSet<String>`; `tool_defs` recomputed each iteration as `tools_for_loop()… .filter(|d| !disabled.contains(&d.name))`; a `ToolResultEntry` for a refusal reads `not authorized: … — `<tool>` is unavailable for the rest of this turn; do not call it again` with `ToolStatus::Denied`; `handle_tool_call` returns, alongside the entry, whether the tool is now disabled (`(ToolResultEntry, bool)` — or set a marker on the entry: add `pub disabled_tool: bool` to `ToolResultEntry` if that struct is runtime-local; prefer the tuple if it is shared with `mur_common::llm`).

### Steps

- [x] **2.1 Write the failing test** — `task_runner.rs` tests, next to `fleet_run_explicit_deny_still_wins`:

```rust
    /// A stub LLM that records the tool names offered on every request.
    struct RecordingLlm {
        inner: crate::llm::stub::SequenceLlm,
        offered: Arc<std::sync::Mutex<Vec<Vec<String>>>>,
    }
    // impl LlmClient for RecordingLlm: push req.tools names, delegate generate/generate_stream to inner
    // (copy the trait surface from CountingToolLlm in this module).

    /// §3.8: a refusal is told once and the tool leaves the list. The stub
    /// asks for fleet_run on three consecutive turns; the tool runs once, the
    /// second and third requests do not offer it, and the reply names the
    /// remedy instead of repeating the attempt.
    #[tokio::test]
    async fn a_refused_tool_is_offered_once_and_then_withdrawn() {
        let offered = Arc::new(std::sync::Mutex::new(Vec::new()));
        let responses = vec![
            fleet_run_call_response("fr-0"),
            fleet_run_call_response("fr-1"),
            fleet_run_call_response("fr-2"),
            end_turn_response("gave up"),
        ];
        let calls = Arc::new(AtomicU64::new(0));
        let runner = Arc::new(
            TaskRunner::with_llm(Arc::new(RecordingLlm { inner: crate::llm::stub::SequenceLlm::new(responses), offered: offered.clone() }))
                .with_tools(vec![Arc::new(RefusingFleetRunTool { calls: calls.clone() })]) // execute → Err(NotAuthorized(not_authorized("fleet_run: …")))
                .with_tools_policy(vec![mur_common::agent::ToolRule { pattern: FLEET_RUN.into(), policy: ToolPolicy::Allow, risk: None }])
                .with_pending_approvals(empty_pending_approvals())
                .with_notifier(tokio::sync::mpsc::channel(16).0)
                .with_hitl_timeout_secs(1)
                .with_iteration_ceiling(6),
        );
        let out = runner.run_sync(loop_spec("refused")).await;
        assert_eq!(calls.load(Ordering::Relaxed), 1, "the refused tool ran exactly once");
        let offered = offered.lock().unwrap();
        assert!(offered[0].iter().any(|n| n == FLEET_RUN), "offered on the first request");
        assert!(offered[1..].iter().all(|names| !names.iter().any(|n| n == FLEET_RUN)), "withdrawn afterwards: {offered:?}");
        let text = last_agent_text(&out);
        assert!(text.contains("not authorized") || text.contains("gave up"), "{text}");
    }
```

  Write `RecordingLlm` and `RefusingFleetRunTool` in full (the tool's `execute` returns `Err(ToolError::NotAuthorized(mur_common::authz::not_authorized("fleet_run: test refusal")))` after incrementing `calls`).

- [x] **2.2 Watch it fail** — today `calls == 3` (or the doom-loop guard trips at the third identical call — which is the spec's "not authorized ×3" complaint in miniature).

- [x] **2.3 Implement** — in `run_agentic_loop`: move the `tool_defs` computation inside the `while` loop head and filter by `disabled`:

```rust
        let mut disabled: HashSet<String> = HashSet::new();
        …
        while iteration < self.iteration_ceiling {
            let tool_defs: Vec<_> = self
                .tools_for_loop()
                .iter()
                .map(|t| t.def())
                .filter(|d| crate::tools::suggest::offer_for_streaming(&d.name, streaming))
                .filter(|d| !disabled.contains(&d.name))
                .collect();
```

  In `handle_tool_call`, the `Err(e)` arm of `tool.execute(...)` becomes:

```rust
            Err(crate::tools::ToolError::NotAuthorized(msg)) => (
                format!("{msg} — `{}` is unavailable for the rest of this turn; do not call it again", call.tool_name),
                crate::tools::ToolStatus::Denied { detail: msg },
                true,
                Vec::new(),
            ),
            Err(e) => ( format!("tool error: {e}"), crate::tools::ToolStatus::Failed { exit_code: -1 }, true, Vec::new() ),
```

  and the policy `Deny` arm's content gains the same suffix. Back in the loop, after the results are gathered, `for (call, entry) in … { if matches!(entry.status, ToolStatus::Denied { .. }) && !entry.content.starts_with("unknown tool") { disabled.insert(call.tool_name.clone()); } }` — `unknown tool` is not a refusal (the model invented a name), so it is not withdrawn (there is nothing to withdraw). Put the rule in one small `fn withdraws(entry: &ToolResultEntry) -> bool` with a two-line test so the string check has a name.

- [x] **2.4 Watch it pass**, then the whole crate — the existing `fleet_run_explicit_deny_still_wins` and the doom-loop tests still hold.

- [x] **2.5 fmt + clippy**, then **commit**:

```
feat(runtime): a refused tool is told once and withdrawn for the turn

NotAuthorized and policy Deny mark the tool disabled; the next request
does not offer it, so the model cannot spin on "not authorized" ×3 — the
shape that started the execution-limits spec.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 3 — needs are declared, and checked before the first model call

**Interfaces.**
- Consumes: `Fleet` (mur-common), `DagExecOptions`, `build_channel_delegate_params` (5 args after step 5), `TaskSpec`, `TaskRunner.tools` + `tools_policy`, `effective_tool_policy`.
- Produces:

```rust
// mur_common::fleet::Fleet
#[serde(default, skip_serializing_if = "Vec::is_empty")] pub needs: Vec<String>,
// mur_core::executor::dag::DagExecOptions
pub needs: Vec<String>,                                   // handed to every delegate
fn build_channel_delegate_params(text, cid, child_task_id, key, deadline_secs, needs: &[String])   // adds "needs": [...] when non-empty
// mur_agent_runtime::task_runner::TaskRunner
pub fn missing_tools(&self, needs: &[String]) -> Vec<String>   // not registered, or policy Deny
// A2A: channel/delegate and message/send read `params.needs: [string]`; a miss returns a Failed Task
//      with error code "cannot_start" and message "cannot start: <agent> has no <tool> — mur agent perm tool-allow <agent> <tool>"
```

### Steps

- [x] **3.1 Write the failing tests** —

  `mur-common/src/fleet.rs` (in the `limits_tests` module): a fleet with `needs: [write_file]` round-trips; one without serialises no `needs` key.

  `mur-core/src/executor/dag.rs` tests:

```rust
    #[test]
    fn delegate_params_carry_the_fleets_needs() {
        let p = build_channel_delegate_params("do x", "fleet-dev", "t1", "k1", None, &["write_file".into(), "bash".into()]);
        assert_eq!(p["needs"], serde_json::json!(["write_file", "bash"]));
        let p = build_channel_delegate_params("do x", "fleet-dev", "t1", "k1", None, &[]);
        assert!(p.get("needs").is_none(), "no needs → no key → no preflight (old behaviour)");
    }
```

  `mur-agent-runtime/src/task_runner.rs` tests:

```rust
    /// The inventory the preflight consults is the loop's own: a tool is
    /// missing when it is not registered or its policy is Deny.
    #[test]
    fn missing_tools_reads_the_same_inventory_the_gate_reads() {
        let runner = TaskRunner::with_llm(Arc::new(crate::llm::stub::SequenceLlm::new(vec![])))
            .with_tools(vec![Arc::new(NoopTool::named("write_file")), Arc::new(NoopTool::named("bash"))])
            .with_tools_policy(vec![mur_common::agent::ToolRule { pattern: "bash".into(), policy: ToolPolicy::Deny, risk: None }]);
        assert_eq!(runner.missing_tools(&["write_file".into()]), Vec::<String>::new());
        assert_eq!(runner.missing_tools(&["bash".into(), "edit_file".into(), "write_file".into()]), vec!["bash".to_string(), "edit_file".to_string()]);
    }
```

  (`NoopTool::named` — a tiny test executor returning `"ok"`; if the module already has one under another name, use it.)

  `channel_delegate.rs` tests:

```rust
    /// §3.8 / §7: a coding brief on an agent whose policy denies write_file
    /// fails before the first LLM call, naming the tool and the grant.
    #[tokio::test]
    async fn a_delegate_missing_a_declared_need_fails_before_the_model_is_called() {
        // runner: a CountingToolLlm-style client whose call count must stay 0;
        // tools: NoopTool "write_file" with policy Deny
        // handler.handle(params with "needs": ["write_file"], RequestContext::none())
        // → Ok(task) where task.status.state == Failed, task.error.code == "cannot_start",
        //   task.error.message == "cannot start: specialist has no write_file — mur agent perm tool-allow specialist write_file",
        //   and the LLM call count is 0.
    }
```

  written in full against the module's existing fixture.

- [x] **3.2 Watch them fail.**

- [x] **3.3 Implement** —

  `mur-common/src/fleet.rs`: after `limits`:

```rust
    /// Tools every member needs for this fleet's work, e.g.
    /// `[write_file, edit_file, bash]` for a coding fleet. Checked by each
    /// delegate BEFORE its first model call (spec 2026-09-12 §3.8): a member
    /// whose policy lacks one fails at dispatch with the grant command, not
    /// after burning its budget. Empty = no preflight.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
```

  Every `Fleet { .. }` literal the compiler names gets `needs: vec![]` (the loop_run/dag/daemon/billing/deep-research tests and the Hub — grep `mur-hub-gui/src-tauri/src` even though step 3 found none).

  `dag.rs`: `DagExecOptions.needs: Vec<String>` (Default = empty; also captured into the per-step clone like `deadline_at`); `build_channel_delegate_params(.., needs: &[String])` inserts `"needs": needs` when non-empty; `execute_step` passes `&opts.needs`. `loop_run.rs` and `run.rs` set `needs: fleet.needs.clone()`. `show.rs`: print `needs: write_file, bash` when non-empty (copy the line shape used for `skills`).

  `task_runner.rs`:

```rust
    /// The tools a brief declares it needs that this runtime cannot offer:
    /// not registered, or denied by policy — the same inventory and the same
    /// rule list the gate consults, so preflight and gate cannot disagree.
    pub fn missing_tools(&self, needs: &[String]) -> Vec<String> {
        needs
            .iter()
            .filter(|n| {
                !self.tools.iter().any(|t| t.name() == n.as_str())
                    || effective_tool_policy(&self.tools_policy, n) == mur_common::agent::ToolPolicy::Deny
            })
            .cloned()
            .collect()
    }
```

  `message_send.rs`: `pub(crate) fn declared_needs(p: &Value) -> Vec<String>` (the `needs` array's string items; absent → empty). In both handlers, after the `TaskSpec` is built and before the turn runs:

```rust
        let needs = super::message_send::declared_needs(&p);   // (or `declared_needs(&p)` in message_send itself)
        let missing = self.runner.missing_tools(&needs);
        if let Some(tool) = missing.first() {
            let agent = self.agent.as_str();                      // message_send: self.runner.agent_name()
            let msg = format!("cannot start: {agent} has no {tool} — mur agent perm tool-allow {agent} {tool}");
            let task = failed_task_before_start(task_id.clone(), &message, "cannot_start", &msg);
            return serde_json::to_value(&task).map_err(|e| HandlerError::Internal(e.to_string()));
        }
```

  with `failed_task_before_start` a small free function in `message_send.rs` that builds a `mur_common::a2a::Task` in state `Failed` carrying only the input message and `TaskError { code, message, recoverable: false, details: None }` (mirror how `run_sync` builds a Failed task; grep `TaskState::Failed` in `task_runner.rs` for the shape). `TaskRunner` needs `pub fn agent_name(&self) -> &str` if it has none. `channel_delegate` also appends its usual signed self-reply? No — nothing ran; the router's step fails with the message, which the rail shows.

- [x] **3.4 Watch it pass** — `cargo nextest run -p mur-common --lib -E 'test(/fleet::/)'`, `-p mur-core --lib -E 'test(/executor::dag::|fleet::/)'`, `-p mur-agent-runtime --lib`. Real-machine check (documented): add `needs: [write_file]` to a scratch fleet whose member denies `write_file`; `mur fleet run <f>` → the step fails in under a second with the `cannot start:` line in `mur fleet status`.

- [x] **3.5 fmt + clippy on the four crates; Hub check last**, then **commit**:

```
feat(fleet): declared needs are checked at dispatch, before the first model call

fleet.yaml `needs:` names the tools the work requires; every delegate
receives the list and answers `cannot start: <agent> has no <tool> — mur
agent perm tool-allow <agent> <tool>` from its own inventory and policy
before building a prompt. No needs, no preflight.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
```

---

## Task 4 — one line of doc

- [x] **4.1** `CLAUDE.md`, under the `mur fleet` bullet after the handles line: `- \`needs:\` in fleet.yaml names the tools the work requires; a member missing one fails at dispatch (\`cannot start: … — mur agent perm tool-allow …\`), and any authorization refusal (\`not authorized:\`) withdraws that tool for the rest of the turn instead of being retried.`
- [x] **4.2** `mur verify --file CLAUDE.md` shows no new ❌. Commit `docs: needs and non-retryable authorization, in words`.

## After the last task

- One PR: `feat: fail at dispatch — declared needs preflight, non-retryable authorization (spec step 6)`. No behaviour change for a fleet without `needs:`; state that.
- Real-machine: `develop-rust` and `develop-web` should declare `needs: [write_file, edit_file, bash]` (their members are coders); `deep-research` none.
- This closes the six rollout steps of the spec. Left: Hub 3b (`LimitsPanel`), `update-docs` for `needs:` and the handle-returning tools.
