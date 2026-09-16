# CLI-spawn backend Implementation Plan
> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a turn actually run inside a spawned `claude`: write the per-turn `--mcp-config` naming the shim, spawn the CLI with the verified isolation flags, stream its reply back, and take `claude` off `Activation::Disabled` — the last step being the one that must be earned rather than assumed.

**Architecture:** A fifth `RunnerBackend` variant. The existing four either answer from a stub or drive MUR's own agentic loop; this one hands the loop to the CLI and collects the result, while every tool the CLI calls comes back through the shim to `GuardedToolCall`. Nothing about the in-process path changes.

**Tech stack:** Rust (edition 2024, `mur-common`, `mur-agent-runtime`).

### Global Constraints

- **No second execution path.** The tripwire stays green; the CLI's tools reach MUR only through the shim and `GuardedToolCall`.
- The user's own CLI configuration is never read or written. Every spawn uses the private home.
- `--tools ""`, `--strict-mcp-config` and `--mcp-config` travel together. Dropping `--strict-mcp-config` re-mounts the user's own MCP servers — measured at 44 tools on this machine — none of which pass MUR's gate.
- Fail closed. A spawn that cannot be isolated does not run.

### Depends on

`#1355` (the shim). Task 2 writes a config naming `mcp-shim`, which must exist.

## File structure

| File | Status | Responsibility |
|---|---|---|
| `mur-common/src/cli_backend.rs` | modified | `mcp_config_json()`: the per-turn config, as a pure function. |
| `mur-agent-runtime/src/cli_spawn.rs` | created | Spawn the CLI, stream its `stream-json`, return the reply. |
| `mur-agent-runtime/src/task_runner.rs` | modified | `RunnerBackend::CliSpawn`, plus the socket path the shim needs. |
| `mur-agent-runtime/src/lib.rs` | modified | One `pub mod cli_spawn;` line. |

## The thing to get right before anything else

Three flags are the isolation, and they were measured together (#1339):

| flags | tools the model sees |
|---|---|
| none | built-ins + the user's MCP servers |
| `--tools ""` | **44** — every MCP server in the user's own config |
| `--tools "" --strict-mcp-config --mcp-config <ours>` | exactly ours |

The middle row is why this plan puts the flag list in one constant with a test,
rather than assembling it at the spawn site where a future edit can drop one
and still look right.

---

## Task 1 — The per-turn MCP config, as a pure function

### Interfaces

**Consumes:** `CliBackend` (`mur-common/src/cli_backend.rs`).

**Produces:**
```rust
pub fn mcp_config_json(shim_bin: &str, socket: &Path, task_id: &str) -> serde_json::Value;
pub const ISOLATION_FLAGS: &[&str]; // ["--tools", "", "--strict-mcp-config"]
```

### Steps

- [ ] Add to `mur-common/src/cli_backend.rs`, above the `#[cfg(test)]` module:

```rust
/// The flags that make a spawned CLI see MUR's tools and nothing else.
///
/// One constant, not three arguments assembled at the call site. Measured
/// 2026-09-16: `--tools ""` alone still left 44 tools mounted — every MCP
/// server in the user's own config — and none of those pass MUR's handler,
/// entitlements or HITL gate. `--strict-mcp-config` is what empties the
/// list, so the three travel together or the isolation is not there.
pub const ISOLATION_FLAGS: &[&str] = &["--tools", "", "--strict-mcp-config"];

/// The `--mcp-config` document for one turn.
///
/// Names the shim, the agent socket it dials back on, and the task it
/// belongs to. `task_id` is what binds a spawned `bash` job to an owner and
/// routes an approval prompt, so it is an argument rather than something the
/// shim could infer.
pub fn mcp_config_json(
    shim_bin: &str,
    socket: &std::path::Path,
    task_id: &str,
) -> serde_json::Value {
    serde_json::json!({
        "mcpServers": {
            "mur": {
                "command": shim_bin,
                "args": [
                    "mcp-shim",
                    "--socket", socket.to_string_lossy(),
                    "--task-id", task_id,
                ],
            }
        }
    })
}
```

- [ ] Add these tests inside the existing `mod tests` block:

```rust
    #[test]
    fn the_isolation_flags_stay_together() {
        // Each of the three is load-bearing and the middle row of the table
        // in the plan is why: dropping --strict-mcp-config re-mounts the
        // user's own MCP servers, and the spawn still looks correct.
        assert_eq!(ISOLATION_FLAGS, &["--tools", "", "--strict-mcp-config"]);
    }

    #[test]
    fn the_mcp_config_names_the_shim_the_socket_and_the_task() {
        let v = mcp_config_json(
            "/usr/local/bin/mur_agent_x",
            std::path::Path::new("/tmp/x/agent.sock"),
            "t-9",
        );
        let s = &v["mcpServers"]["mur"];
        assert_eq!(s["command"], "/usr/local/bin/mur_agent_x");
        let args: Vec<String> = s["args"]
            .as_array()
            .expect("args")
            .iter()
            .map(|a| a.as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(args[0], "mcp-shim");
        assert!(args.contains(&"/tmp/x/agent.sock".to_string()));
        assert!(args.contains(&"t-9".to_string()));
    }

    #[test]
    fn the_config_declares_exactly_one_server() {
        // `--strict-mcp-config` means this document is the whole tool
        // surface. A second entry here would be a second unaudited source.
        let v = mcp_config_json("bin", std::path::Path::new("/s"), "t");
        assert_eq!(v["mcpServers"].as_object().expect("obj").len(), 1);
    }
```

- [ ] Run and watch them pass:

```bash
cargo test -p mur-common cli_backend
```

Expected: `test result: ok. 17 passed; 0 failed`.

- [ ] Commit: `git add -A && git commit -m "feat(cli-backend): the per-turn MCP config and the isolation flags"`

---

## Task 2 — Spawn the CLI and read its reply

### Interfaces

**Consumes:** `ISOLATION_FLAGS`, `mcp_config_json`, `CliBackend`, `ensure_home` (Task 1 and existing).

**Produces:**
```rust
pub struct SpawnRequest<'a> {
    pub backend: &'a mur_common::cli_backend::CliBackend,
    pub mur_home: &'a Path,
    pub shim_bin: &'a str,
    pub socket: &'a Path,
    pub task_id: &'a str,
    pub prompt: &'a str,
}
pub async fn run_turn(req: SpawnRequest<'_>) -> anyhow::Result<String>;
pub fn reply_from_stream_json(lines: &str) -> String;
```

`reply_from_stream_json` is separate and pure so the envelope handling is
testable without spawning anything.

### Steps

- [ ] Create `mur-agent-runtime/src/cli_spawn.rs`:

```rust
//! Run one turn inside a spawned coding CLI.
//!
//! The CLI owns the agentic loop; MUR owns the tools, which reach it through
//! the shim named in the per-turn `--mcp-config`. Nothing here executes a
//! tool, and nothing here reads the user's own CLI configuration: the spawn
//! runs against this backend's private home.
//!
//! See `docs/superpowers/specs/2026-09-16-cli-spawn-backends-design.md`.

use mur_common::cli_backend::{CliBackend, ISOLATION_FLAGS, ensure_home, mcp_config_json};
use std::path::Path;

pub struct SpawnRequest<'a> {
    pub backend: &'a CliBackend,
    pub mur_home: &'a Path,
    pub shim_bin: &'a str,
    pub socket: &'a Path,
    pub task_id: &'a str,
    pub prompt: &'a str,
}

/// The assistant text from a `stream-json` transcript.
///
/// Concatenates every assistant text block in order. Tool traffic is not
/// included: those calls already ran through MUR's handler and were recorded
/// there, so repeating them here would double-count a turn's history.
pub fn reply_from_stream_json(lines: &str) -> String {
    let mut out = String::new();
    for line in lines.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if v["type"] != "assistant" {
            continue;
        }
        let Some(blocks) = v["message"]["content"].as_array() else {
            continue;
        };
        for b in blocks {
            if b["type"] == "text"
                && let Some(t) = b["text"].as_str()
            {
                out.push_str(t);
            }
        }
    }
    out
}

pub async fn run_turn(req: SpawnRequest<'_>) -> anyhow::Result<String> {
    let (home_var, home) = ensure_home(req.mur_home, req.backend)?;
    let cfg_path = home.join("mur-mcp.json");
    std::fs::write(
        &cfg_path,
        serde_json::to_vec_pretty(&mcp_config_json(req.shim_bin, req.socket, req.task_id))?,
    )?;

    let mut cmd = tokio::process::Command::new(req.backend.binary);
    cmd.args(req.backend.headless_invocation)
        .args(req.backend.stream_flags)
        .args(ISOLATION_FLAGS)
        .arg("--mcp-config")
        .arg(&cfg_path)
        // The private home, so the spawn never reads or writes the user's own
        // CLI configuration.
        .env(home_var, &home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn()?;
    {
        use tokio::io::AsyncWriteExt;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("no stdin"))?;
        stdin.write_all(req.prompt.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        // Dropped here: the CLI reads its prompt to EOF, so holding this open
        // would leave it waiting for input that is never coming.
    }

    let out = child.wait_with_output().await?;
    if !out.status.success() {
        anyhow::bail!(
            "{} exited {}: {}",
            req.backend.binary,
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(reply_from_stream_json(&String::from_utf8_lossy(
        &out.stdout,
    )))
}
```

- [ ] Add `pub mod cli_spawn;` to `mur-agent-runtime/src/lib.rs` in alphabetical
      position.

- [ ] Add the test module to the bottom of `cli_spawn.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// A real `claude --output-format stream-json` transcript shape.
    const TRANSCRIPT: &str = r#"{"type":"system","subtype":"init","tools":[]}
{"type":"assistant","message":{"content":[{"type":"text","text":"Hello"}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","name":"mcp__mur__bash"}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":" and goodbye"}]}}
{"type":"result","subtype":"success"}"#;

    #[test]
    fn the_reply_is_the_assistant_text_in_order() {
        assert_eq!(reply_from_stream_json(TRANSCRIPT), "Hello and goodbye");
    }

    #[test]
    fn tool_traffic_is_not_part_of_the_reply() {
        // Those calls already ran through MUR's handler and were recorded
        // there. Repeating them would double-count the turn's history.
        assert!(!reply_from_stream_json(TRANSCRIPT).contains("bash"));
    }

    #[test]
    fn a_malformed_line_does_not_discard_the_rest() {
        let s = format!("not json\n{TRANSCRIPT}");
        assert_eq!(reply_from_stream_json(&s), "Hello and goodbye");
    }

    #[test]
    fn an_empty_transcript_is_an_empty_reply_not_a_panic() {
        assert_eq!(reply_from_stream_json(""), "");
    }
}
```

- [ ] Run and watch them pass:

```bash
cargo test -p mur-agent-runtime cli_spawn
```

Expected: `test result: ok. 4 passed; 0 failed`.

- [ ] Commit: `git add -A && git commit -m "feat(runtime): run a turn inside a spawned CLI"`

---

## Task 3 — Wire it to a turn, and earn the activation

### Interfaces

**Consumes:** `run_turn` (Task 2).

**Produces:**
```rust
// task_runner.rs
pub enum RunnerBackend { /* … */ CliSpawn(&'static CliBackend) }
impl TaskRunner { pub fn with_socket_path(self, p: PathBuf) -> Self }
```

### Steps

- [ ] Add the variant to `RunnerBackend` in `mur-agent-runtime/src/task_runner.rs`:

```rust
    /// The turn runs inside a spawned coding CLI, which owns the loop while
    /// MUR owns the tools. Not an `LlmClient`: there is no completion call to
    /// make here, so it cannot be one of those.
    CliSpawn(&'static mur_common::cli_backend::CliBackend),
```

- [ ] Add the socket field and its builder, beside the other `with_*` methods:

```rust
    /// The agent's own socket, so a spawned CLI's shim can dial back in.
    ///
    /// Carried rather than derived: the runner has no profile, and the bound
    /// path is not always the canonical one — `socket_path::resolve_bind_target`
    /// relocates a long path to /tmp and symlinks it.
    pub fn with_socket_path(mut self, p: std::path::PathBuf) -> Self {
        self.socket_path = Some(p);
        self
    }
```

Add `socket_path: Option<std::path::PathBuf>` to the struct and `socket_path:
None` to its constructor, beside the other optional fields.

- [ ] Add the dispatch arm, after `RunnerBackend::Misconfigured`:

```rust
                RunnerBackend::CliSpawn(backend) => {
                    let Some(socket) = self.socket_path.clone() else {
                        // Fail closed and say why: without the socket the
                        // shim cannot dial back, so the CLI would run with
                        // no MUR tools at all — a working-looking turn with
                        // none of the guarantees.
                        return Ok((
                            text_response(
                                "cli-spawn backend has no agent socket configured; refusing to spawn",
                            ),
                            None,
                        ));
                    };
                    let shim = std::env::current_exe()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| "mur-agent-runtime".to_string());
                    let reply = crate::cli_spawn::run_turn(crate::cli_spawn::SpawnRequest {
                        backend,
                        mur_home: &mur_common::trust::mur_home(),
                        shim_bin: &shim,
                        socket: &socket,
                        task_id: &spec.task_id,
                        prompt: &text_of(&spec.input),
                    })
                    .await
                    .map_err(|e| task_error("cli_spawn_failed", &format!("{e}")))?;
                    Ok((text_response(&reply), None))
                }
```

- [ ] If `spec.task_id` does not exist under that name, use whatever the
      surrounding arms use to identify the turn. Do not invent one: the id
      that reaches the shim must be the same id the gate routes by, or an
      approval prompt goes to nobody.

- [ ] Build:

```bash
cargo build -p mur-agent-runtime
```

Expected: compiles.

- [ ] **Earn the activation.** Only now change the `claude` row in
      `mur-common/src/cli_backend.rs`, and only if every box below is ticked:

```rust
    activation: Activation::Enabled,
```

  - [ ] `cargo test -p mur-agent-runtime execute_is_called_from_guarded_only` passes.
  - [ ] `grep -c ToolExecutor mur-agent-runtime/src/cli_spawn.rs` is `0`.
  - [ ] A spawn's `system init` reports MUR's tools **and nothing else**
        (`--output-format stream-json --verbose`, read the `tools` array).
  - [ ] The user's `~/.claude` is byte-identical before and after a spawn.

- [ ] Update the test that pinned the old reason — it asserts
      `reason.contains("spawn path")`, which no longer holds:

```rust
    #[test]
    fn claude_is_enabled_once_the_spawn_path_exists() {
        // The gate is not "a probe answered"; it is that every isolation
        // requirement was demonstrated. The boxes are in the plan.
        assert!(matches!(CLAUDE.activation, Activation::Enabled));
    }
```

- [ ] End-to-end, by hand, against a running agent:

```bash
MUR_AGENT=mur
mur agent send $MUR_AGENT "say PROBE_OK and nothing else"
```

Expected: `PROBE_OK`, produced by a spawned `claude` rather than by MUR's own
loop. Confirm which by checking that `~/.mur/cli-homes/claude/mur-mcp.json`
was written during the turn.

- [ ] Lint and format:

```bash
cargo clippy --all --all-targets --no-deps -- -D warnings && cargo fmt --check
```

- [ ] Commit: `git add -A && git commit -m "feat(runtime): CliSpawn backend, and claude is enabled"`

## Done when

- [ ] `cargo test -p mur-common cli_backend` — 17 passed.
- [ ] `cargo test -p mur-agent-runtime cli_spawn` — 4 passed.
- [ ] `execute_is_called_from_guarded_only` — passes.
- [ ] A real spawn answers, with MUR's tools and only MUR's tools mounted.
- [ ] `~/.claude` unchanged by a spawn.
- [ ] `cargo clippy --all --all-targets --no-deps -- -D warnings` — clean.
      `--all-targets` is load-bearing: without it clippy never reads test
      code, which is how #1355 passed locally and failed CI.

## Not in this plan

- **Streaming the CLI's output to a channel.** `run_turn` collects and returns.
  Deltas are a second change: `stream-json` carries them, and the plumbing to
  a channel notifier is its own decision about what a spawned turn's step
  events mean — open question 2 of the MCP server design.
- **`codex` and `agy`.** Neither has a registry row, both mount MCP
  persistently, and `agy` has 57 built-ins with no way to disable them.
- **Choosing the backend per agent.** Nothing constructs `RunnerBackend::CliSpawn`
  yet; wiring it to a profile field is the next change, and it is where the
  user-facing decision of *which* agents use this track gets made.
