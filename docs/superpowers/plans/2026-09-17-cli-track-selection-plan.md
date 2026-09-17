# Choosing the CLI track Implementation Plan
> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let an agent actually be put on the CLI-spawn track — a model registry entry with `provider: claude-cli` makes its turns run inside a spawned `claude` — and give `TaskRunner::with_socket_path` its first caller, which it does not currently have.

**Architecture:** The gateway track is already selected by a provider string (`claude` / `codex` dispatch to gateway clients in `build_client_from_entry`). The CLI track uses the same mechanism and the same place — except it must short-circuit *before* the client is built, because a CLI-spawn turn is not an `LlmClient` at all: it replaces the loop rather than answering one call inside it.

**Tech stack:** Rust (edition 2024, `mur-common`, `mur-agent-runtime`).

### Global Constraints

- **No second execution path.** The tripwire stays green.
- Fail closed and say why. An agent pointed at a CLI backend that is disabled, absent, or unreachable must produce a turn that *explains itself*, never a silent echo and never an ungated spawn.
- Nothing about the gateway track or the in-process path changes behaviour.

### The gap this closes

`RunnerBackend::CliSpawn` exists and works (#1372), and `with_socket_path`
exists to feed it — but `grep -rn "with_socket_path"` finds **no caller**, so
the arm would hit its own fail-closed branch on every turn. The type is
reachable; the configuration is not.

## File structure

| File | Status | Responsibility |
|---|---|---|
| `mur-common/src/cli_backend.rs` | modified | `from_provider()`: the provider string → backend mapping, and the `claude-cli` name. |
| `mur-agent-runtime/src/task_runner.rs` | modified | `TaskRunner::with_cli_spawn`, beside `with_llm`. |
| `mur-agent-runtime/src/supervisor_runner.rs` | modified | Short-circuit to the CLI track before building a client; pass the socket. |

---

## Task 1 — The provider string maps to a backend

### Interfaces

**Consumes:** `CliBackend`, `REGISTRY`, `Activation` (existing).

**Produces:**
```rust
pub const PROVIDER_PREFIX: &str = "cli:";
pub fn from_provider(provider: &str) -> Option<&'static CliBackend>;
```

`cli:claude` rather than `claude-cli`: the prefix makes the track visible at a
glance and cannot collide with a vendor slug, where a suffix would have to be
parsed off a name that is itself allowed to contain dashes.

### Steps

- [ ] Add to `mur-common/src/cli_backend.rs`, above the `#[cfg(test)]` module:

```rust
/// Marks a model registry `provider` as naming the CLI-spawn track.
///
/// The gateway track already selects on `provider` (`claude` and `codex`
/// dispatch to loopback clients), so the CLI track uses the same field
/// rather than inventing a second way to say which track an agent is on.
/// The prefix keeps the two readable side by side — `claude` is the
/// gateway, `cli:claude` is the spawn — and cannot be confused with a
/// vendor slug, which a `-cli` suffix would have to be parsed off a name
/// that may itself contain dashes.
pub const PROVIDER_PREFIX: &str = "cli:";

/// The backend a registry `provider` names, if it names one.
///
/// Returns the row whether or not it is enabled. Activation is the caller's
/// gate to apply and to report: a disabled backend must produce a turn that
/// explains itself, which it cannot do if this returns `None` and the
/// provider merely looks unknown.
pub fn from_provider(provider: &str) -> Option<&'static CliBackend> {
    let key = provider.strip_prefix(PROVIDER_PREFIX)?;
    backend(key)
}
```

- [ ] Add these tests inside the existing `mod tests` block:

```rust
    #[test]
    fn a_prefixed_provider_names_the_backend() {
        assert_eq!(from_provider("cli:claude").map(|b| b.key), Some("claude"));
    }

    #[test]
    fn the_gateway_providers_are_not_the_cli_track() {
        // `claude` and `codex` already mean the loopback gateway. If this
        // ever matched them, putting an agent on the gateway would silently
        // spawn a CLI instead.
        assert!(from_provider("claude").is_none());
        assert!(from_provider("codex").is_none());
        assert!(from_provider("openai").is_none());
    }

    #[test]
    fn an_unknown_backend_is_none_even_when_prefixed() {
        assert!(from_provider("cli:agy").is_none());
        assert!(from_provider("cli:nope").is_none());
    }

    #[test]
    fn a_disabled_backend_still_resolves() {
        // The caller needs the row to report *why* it is off. Returning
        // `None` would make a disabled backend indistinguishable from a typo.
        let disabled = CliBackend {
            activation: Activation::Disabled {
                reason: "for the test",
            },
            ..CLAUDE
        };
        assert!(matches!(disabled.activation, Activation::Disabled { .. }));
        assert!(from_provider("cli:claude").is_some());
    }
```

- [ ] Run and watch them pass:

```bash
cargo test -p mur-common cli_backend
```

Expected: `test result: ok. 21 passed; 0 failed`.

- [ ] Commit: `git add -A && git commit -m "feat(cli-backend): cli: provider prefix selects the spawn track"`

---

## Task 2 — A runner on the CLI track

### Interfaces

**Consumes:** `RunnerBackend::CliSpawn`, `with_socket_path` (both #1372).

**Produces:**
```rust
impl TaskRunner {
    pub fn with_cli_spawn(b: &'static mur_common::cli_backend::CliBackend) -> Self
}
```

### Steps

- [ ] Add to `mur-agent-runtime/src/task_runner.rs`, directly after
      `with_llm`:

```rust
    /// A runner whose turns run inside a spawned CLI.
    ///
    /// Beside `with_llm` rather than derived from it: this backend has no
    /// `LlmClient` to hold. The CLI owns the loop, so there is no completion
    /// call for MUR to make.
    ///
    /// The socket is **not** optional in practice — without it the dispatch
    /// arm refuses rather than spawning a CLI that cannot reach MUR's tools —
    /// but it is set separately by `with_socket_path`, because the value
    /// comes from the profile's transport config and this constructor is
    /// called from places that have the backend before they have the socket.
    pub fn with_cli_spawn(b: &'static mur_common::cli_backend::CliBackend) -> Self {
        Self::with_backend(RunnerBackend::CliSpawn(b))
    }
```

- [ ] Build:

```bash
cargo build -p mur-agent-runtime
```

Expected: compiles.

- [ ] Commit: `git add -A && git commit -m "feat(runtime): TaskRunner::with_cli_spawn"`

---

## Task 3 — Route an agent onto the track

This is where the fail-closed rule earns its place: four ways to be on the
CLI track and not able to use it, each of which must produce a turn that says
so rather than a turn that looks fine.

### Interfaces

**Consumes:** `from_provider` (Task 1), `with_cli_spawn` (Task 2),
`profile.inner.transport.socket.bind`.

**Produces:** no new API — `build_provider_runner` gains a branch.

### Steps

- [ ] Find the point in `mur-agent-runtime/src/supervisor_runner.rs` where
      the resolved registry entry's provider is about to be turned into a
      client (the call into `build_client_from_entry`, or the code that
      decides to). Insert the branch **before** it. The provider string is on
      the resolved entry; if the surrounding code names it differently, use
      that name — do not add a second lookup.

```rust
    // The CLI track short-circuits before any client is built: a spawned CLI
    // owns the loop, so there is no `LlmClient` to construct. Checked here
    // rather than inside `build_client_from_entry`, which can only return a
    // client and so has no way to express this backend.
    if let Some(backend) = mur_common::cli_backend::from_provider(&entry.provider) {
        return Ok((
            Arc::new(cli_spawn_runner(backend, profile, /* the same args the
                     LLM path passes: tools, policy, secrets, … */)),
            None,
            None,
            None,
        ));
    }
```

- [ ] Write the helper beside `build_runner` in the same file. It applies the
      gate before it applies the configuration, so a refusal cannot be
      mistaken for a working agent:

```rust
/// A runner for the CLI-spawn track, or a misconfigured one that says why.
///
/// Every refusal here produces a turn the user can read. The alternative —
/// falling back to the echo stub — looks alive and parrots, which is the
/// failure `RunnerBackend::Misconfigured` was introduced to end.
fn cli_track_runner(
    backend: &'static mur_common::cli_backend::CliBackend,
    socket_bind: &str,
) -> TaskRunner {
    use mur_common::cli_backend::Activation;
    if let Activation::Disabled { reason } = backend.activation {
        return TaskRunner::new_stub_misconfigured(format!(
            "agent is on the `{}{}` track, which is disabled: {reason}",
            mur_common::cli_backend::PROVIDER_PREFIX,
            backend.key
        ));
    }
    let socket = socket_bind.trim_start_matches("unix://");
    if socket.is_empty() {
        return TaskRunner::new_stub_misconfigured(format!(
            "agent is on the `{}{}` track but has no unix socket configured; \
             the spawned CLI would have no way to reach MUR's tools",
            mur_common::cli_backend::PROVIDER_PREFIX,
            backend.key
        ));
    }
    TaskRunner::with_cli_spawn(backend).with_socket_path(std::path::PathBuf::from(socket))
}
```

- [ ] Apply the same `with_*` configuration the LLM path applies — tools,
      policy, secrets, pending approvals, notifier, limits. A CLI-track turn
      runs the *same* tools through the *same* gate; a runner missing its
      policy would spawn a CLI whose tool calls arrive unpoliced. If the
      existing code builds that chain in one place, call it; if it is inline,
      factor it rather than copying it, and say so in the commit.

- [ ] Add these tests beside the existing `supervisor_runner` tests:

```rust
    #[test]
    fn a_disabled_backend_produces_a_turn_that_explains_itself() {
        use mur_common::cli_backend::{Activation, CLAUDE, CliBackend};
        static DISABLED: CliBackend = CliBackend {
            activation: Activation::Disabled { reason: "probe pending" },
            ..CLAUDE
        };
        let r = cli_track_runner(&DISABLED, "unix:///tmp/a.sock");
        // The stub's message is the turn's whole reply, so asserting it is
        // asserting what the user sees.
        assert!(matches!(r.backend_for_test(), RunnerBackend::Misconfigured(m)
            if m.contains("disabled") && m.contains("probe pending")));
    }

    #[test]
    fn no_socket_refuses_rather_than_spawning_blind() {
        use mur_common::cli_backend::CLAUDE;
        let r = cli_track_runner(&CLAUDE, "");
        assert!(matches!(r.backend_for_test(), RunnerBackend::Misconfigured(m)
            if m.contains("no unix socket")));
    }

    #[test]
    fn a_usable_backend_gets_the_socket_it_will_dial() {
        use mur_common::cli_backend::CLAUDE;
        let r = cli_track_runner(&CLAUDE, "unix:///tmp/a.sock");
        // The `unix://` prefix must be stripped: it is a config spelling, not
        // a filesystem path, and `UnixStream::connect` takes the latter.
        assert_eq!(r.socket_path_for_test(), Some(std::path::Path::new("/tmp/a.sock")));
    }
```

- [ ] These tests need two read-only accessors. Add them next to the other
      `#[cfg(test)]` helpers in `task_runner.rs`, not as public API:

```rust
    #[cfg(test)]
    pub(crate) fn backend_for_test(&self) -> &RunnerBackend {
        &self.backend
    }

    #[cfg(test)]
    pub(crate) fn socket_path_for_test(&self) -> Option<&std::path::Path> {
        self.socket_path.as_deref()
    }
```

- [ ] Run them and watch them pass:

```bash
cargo test -p mur-agent-runtime supervisor_runner
```

Expected: the three new tests pass alongside the existing ones.

- [ ] Confirm the hard constraint still holds:

```bash
cargo test -p mur-agent-runtime execute_is_called_from_guarded_only
```

Expected: `test result: ok. 1 passed; 0 failed`.

- [ ] Lint and format with CI's own invocation:

```bash
cargo clippy --all --all-targets --no-deps -- -D warnings && cargo fmt --check
```

- [ ] Commit: `git add -A && git commit -m "feat(runtime): route an agent onto the CLI track by provider"`

## Done when

- [ ] `cargo test -p mur-common cli_backend` — 21 passed.
- [ ] `cargo test -p mur-agent-runtime supervisor_runner` — the three new tests pass.
- [ ] `execute_is_called_from_guarded_only` — passes.
- [ ] `grep -rn "with_socket_path" mur-agent-runtime/src/` finds a caller.
      That absence is the gap this plan exists to close.
- [ ] `cargo clippy --all --all-targets --no-deps -- -D warnings && cargo fmt --check` — clean.

## Not in this plan

- **A CLI command to put an agent on the track.** The selector is a model
  registry entry, so `mur model add --provider cli:claude …` already reaches
  it; whether that deserves a friendlier surface is a UX decision, not this
  change.
- **Streaming the spawned CLI's output.** `run_turn` still collects and
  returns; deltas are open question 2 of the MCP server design.
- **`codex` and `agy`.** Neither has a registry row, so `from_provider`
  returns `None` for both and an agent pointed at them gets the ordinary
  unknown-provider path.
- **Documentation.** A new provider spelling is user-facing; README, the docs
  site and the product page are the `update-docs` skill's job once this
  works end to end.
