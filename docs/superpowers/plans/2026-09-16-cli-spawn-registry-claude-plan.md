# CLI-spawn backends — registry + claude slice Implementation Plan
> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the data layer and the user-facing availability logic of the CLI-spawn track — a backend registry, binary-presence availability, the private CLI home, and the Hub's two-track selection — with `claude` registered as **visible but disabled** because the spec's activation rule forbids enabling a backend whose tool-isolation is unverified.

**Architecture:** A static registry in `mur-common` describes each CLI-spawn backend as data (binary, invocation, flags, MCP mount style, home env var, activation), so adding or retiring a backend is a row rather than a code path. Availability is computed from an injected binary resolver, keeping it testable without touching `PATH`. The Hub consumes gateway hook state and CLI gate state and returns which of the two tracks to offer, implementing the spec's selection table verbatim.

**Tech stack:** Rust (edition 2024, `mur-common`), TypeScript + Vitest (`mur-hub-gui/ui`).

### Global Constraints

Copied verbatim from `docs/superpowers/specs/2026-09-16-cli-spawn-backends-design.md`. Every task implicitly includes all of them.

- **The existing gateway-hook track is unchanged.**
- Nothing in `mur-model-gateway`, `llm/codex.rs`, `llm/claude.rs` or `llm/loopback.rs` changes behaviour.
- The user's own CLI configuration is never read or written.
- Backends whose binary is absent do not appear in the UI at all.
- Failure or unknown results keep the backend disabled; binary presence is insufficient.
- `unknown` never disables a control and never routes silently to CLI spawn.
- `false` means "this build denied it"; `unknown` means "could not ask" and must never be folded into `false`.

### Out of scope, and why

Not laziness — these cannot be written as mechanical steps today:

- **Spawning the CLI and mounting MUR's tools.** The spec's chosen design requires MUR to *serve* its tools over MCP. `mur-agent-runtime/src/mcp/` is client-only (`pool.rs` is a pool of `McpClient`); no server exists, and the spec designs it in one sentence. Needs its own design before it can be planned.
- **`codex` and `agy` rows.** Open questions 1–3 leave `agy`'s home env var unidentified and both streaming envelopes unverified. A row with an unknown field is the placeholder this plan format forbids.
- **Enabling `claude`.** Open question 5: `--disallowedTools` scope is unverified. The Global Constraint above makes `Activation::Disabled` the only correct initial value.

## File structure

| File | Status | Responsibility |
|---|---|---|
| `mur-common/src/cli_backend.rs` | created | The registry: backend record, activation, the `claude` row, lookup, availability, private-home paths. Nothing spawns here. |
| `mur-common/src/lib.rs` | modified | One `pub mod cli_backend;` line. |
| `mur-hub-gui/ui/src/components/cliTrack.ts` | created | Pure model: `(hook state, CLI gate) -> which tracks to offer`. No React, no Tauri. |
| `mur-hub-gui/ui/src/components/cliTrack.test.ts` | created | One test per row of the spec's selection table, plus the two invariants. |

---

## Task 1 — Backend registry record and the `claude` row

### Interfaces

**Consumes:** nothing (first task).

**Produces:**
```rust
pub enum McpMount { PerCall, Persistent }
pub enum Activation { Enabled, Disabled { reason: &'static str } }
pub struct CliBackend {
    pub key: &'static str,
    pub binary: &'static str,
    pub headless_invocation: &'static [&'static str],
    pub stream_flags: &'static [&'static str],
    pub tool_disable_flags: &'static [&'static str],
    pub mcp_mount: McpMount,
    pub home_env_var: &'static str,
    pub activation: Activation,
    pub capability_notes: &'static str,
}
pub const CLAUDE: CliBackend;
pub const REGISTRY: &[CliBackend];
pub fn backend(key: &str) -> Option<&'static CliBackend>;
```

### Steps

- [ ] Create `mur-common/src/cli_backend.rs` containing exactly this, and nothing else yet:

```rust
//! CLI-spawn backend registry.
//!
//! One row per coding CLI MUR can drive. The registry is data, not code
//! paths: Gemini CLI was replaced by Antigravity inside a year, and the
//! design records that hardcoding per-CLI flags guarantees rewriting this on
//! the next replacement.
//!
//! A row exists only when every field is known. `agy`'s home env var and the
//! `codex` / `agy` streaming envelopes are still open questions in
//! `docs/superpowers/specs/2026-09-16-cli-spawn-backends-design.md`, so those
//! rows are absent rather than half-filled — an unknown field here would be
//! read as fact by every consumer.
//!
//! Nothing in this module spawns a process or reads the user's CLI config.

/// How a backend's MCP configuration reaches the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpMount {
    /// Flags on every invocation; nothing is written to disk.
    PerCall,
    /// Written once into the backend's private home.
    Persistent,
}

/// Whether MUR may actually drive this backend.
///
/// Separate from binary presence on purpose. The spec's activation gate reads:
/// "Failure or unknown results keep the backend disabled; binary presence is
/// insufficient." A disabled backend is still listed and still rendered — it
/// is the panel that carries the explanation and the controls, so hiding it
/// would strand the user exactly as hiding a subscription provider did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activation {
    Enabled,
    /// `reason` is shown to the user. It names the unmet requirement.
    Disabled {
        reason: &'static str,
    },
}

/// One CLI-spawn backend, as data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliBackend {
    /// Stable identifier used in paths and UI keys.
    pub key: &'static str,
    /// Executable name, resolved against the user's shell PATH by the caller.
    pub binary: &'static str,
    /// Flags that put the CLI in headless mode.
    pub headless_invocation: &'static [&'static str],
    /// Flags that select a machine-readable streaming envelope.
    pub stream_flags: &'static [&'static str],
    /// Flags that disable the CLI's own built-in tools.
    pub tool_disable_flags: &'static [&'static str],
    pub mcp_mount: McpMount,
    /// Environment variable that relocates this CLI's home.
    pub home_env_var: &'static str,
    pub activation: Activation,
    /// Free text for the panel: what is known, and what is not.
    pub capability_notes: &'static str,
}

/// `claude`, measured at 2.1.273 by running it, not only by reading `--help`.
///
/// `tool_disable_flags` is `--tools ""`, not `--disallowedTools`: the latter
/// is a named deny list, and denying `Bash` merely sent the model to `Glob`.
///
/// Both halves of the disable are required. `--tools ""` alone left 44 tools
/// mounted — every MCP server in the user's own config — so the flags below
/// are the pair, and `--strict-mcp-config` is load-bearing. Verified from the
/// `system init` event's `tools` array, which reported `[]`.
///
/// Still disabled, for a different reason than the earlier draft: the probe
/// is answered, but nothing can spawn this yet. MUR does not serve its tools
/// over MCP, so there is no loop for the CLI to call back into.
pub const CLAUDE: CliBackend = CliBackend {
    key: "claude",
    binary: "claude",
    headless_invocation: &["-p"],
    stream_flags: &["--output-format", "stream-json"],
    tool_disable_flags: &["--tools", "", "--strict-mcp-config"],
    mcp_mount: McpMount::PerCall,
    home_env_var: "CLAUDE_CONFIG_DIR",
    activation: Activation::Disabled {
        reason: "spawn path not implemented: MUR does not yet serve its tools over MCP",
    },
    capability_notes: "--tools \"\" disables built-ins but NOT the user's own MCP \
                       servers; --strict-mcp-config is what empties the tool list",
};

/// Every backend whose record is complete. Absence is a statement: a CLI
/// missing here has an unanswered probe, not a missing implementation.
pub const REGISTRY: &[CliBackend] = &[CLAUDE];

/// Look up a backend by key.
pub fn backend(key: &str) -> Option<&'static CliBackend> {
    REGISTRY.iter().find(|b| b.key == key)
}
```

- [ ] Add the module to `mur-common/src/lib.rs`. The list is alphabetical; insert the line between `pub mod channel;` and `pub mod commander;`:

```rust
pub mod cli_backend;
```

- [ ] Append the test module to the bottom of `mur-common/src/cli_backend.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_row_matches_the_measured_capabilities() {
        assert_eq!(CLAUDE.binary, "claude");
        assert_eq!(CLAUDE.headless_invocation, &["-p"]);
        assert_eq!(CLAUDE.stream_flags, &["--output-format", "stream-json"]);
        assert_eq!(
            CLAUDE.tool_disable_flags,
            &["--tools", "", "--strict-mcp-config"]
        );
        assert_eq!(CLAUDE.mcp_mount, McpMount::PerCall);
        assert_eq!(CLAUDE.home_env_var, "CLAUDE_CONFIG_DIR");
    }

    #[test]
    fn claude_is_disabled_because_nothing_can_spawn_it_yet() {
        // The activation gate, as a test. The blocker is no longer the tool
        // probe — that is answered — but the missing spawn path.
        match CLAUDE.activation {
            Activation::Disabled { reason } => assert!(reason.contains("spawn path")),
            Activation::Enabled => panic!("nothing can spawn a backend yet"),
        }
    }

    #[test]
    fn the_tool_disable_carries_both_halves() {
        // Regression guard for the measured hazard: `--tools ""` on its own
        // left 44 of the user's own MCP tools mounted. Dropping
        // --strict-mcp-config here would silently reopen that hole.
        assert!(CLAUDE.tool_disable_flags.contains(&"--tools"));
        assert!(CLAUDE.tool_disable_flags.contains(&"--strict-mcp-config"));
        assert!(
            !CLAUDE.tool_disable_flags.contains(&"--disallowedTools"),
            "--disallowedTools is a named deny list, not a disable"
        );
    }

    #[test]
    fn every_row_is_fully_specified() {
        // The rule the registry exists to enforce: no half-filled row. A
        // backend with an unknown field belongs outside the registry, not
        // inside it with a plausible-looking guess.
        for b in REGISTRY {
            assert!(!b.key.is_empty(), "{}: empty key", b.key);
            assert!(!b.binary.is_empty(), "{}: empty binary", b.key);
            assert!(
                !b.headless_invocation.is_empty(),
                "{}: no headless flags",
                b.key
            );
            assert!(!b.stream_flags.is_empty(), "{}: no stream flags", b.key);
            assert!(!b.home_env_var.is_empty(), "{}: no home env var", b.key);
        }
    }

    #[test]
    fn unprobed_backends_are_absent_rather_than_guessed() {
        assert!(
            backend("agy").is_none(),
            "agy's home env var is open question 1"
        );
        assert!(
            backend("codex").is_none(),
            "codex's stream envelope is open question 2"
        );
    }

    #[test]
    fn lookup_finds_claude_and_rejects_unknown_keys() {
        assert_eq!(backend("claude"), Some(&CLAUDE));
        assert!(backend("nope").is_none());
    }
}
```

- [ ] Run the tests and watch them pass:

```bash
cargo test -p mur-common cli_backend
```

Expected: `test result: ok. 6 passed; 0 failed`.

- [ ] Run lint and format:

```bash
cargo clippy -p mur-common -- -D warnings && cargo fmt --check
```

Expected: no output, exit 0.

- [ ] Commit: `git add mur-common/src/cli_backend.rs mur-common/src/lib.rs && git commit -m "feat(cli-backend): registry record and the claude row"`

---

## Task 2 — Availability from binary presence, without hiding a disabled backend

Two rules meet here and they pull in opposite directions, which is why this is its own task with its own tests:

- "Backends whose binary is absent do not appear in the UI at all." — absent binary means **gone**.
- "Failure or unknown results keep the backend disabled" — unverified means **shown, disabled**.

### Interfaces

**Consumes:** `CliBackend`, `REGISTRY`, `Activation` (Task 1).

**Produces:**
```rust
pub struct BackendAvailability {
    pub backend: &'static CliBackend,
    pub path: std::path::PathBuf,
    pub usable: bool,
}
pub fn available<F>(resolve: F) -> Vec<BackendAvailability>
where F: Fn(&str) -> Option<std::path::PathBuf>;
```

### Steps

- [ ] Add to `mur-common/src/cli_backend.rs`, directly above the `#[cfg(test)]` module:

```rust
/// A backend whose binary was found, plus whether MUR may drive it.
///
/// `usable == false` is a backend that is present and listed but must not be
/// spawned; the caller renders it with `Activation::Disabled`'s reason. It is
/// deliberately not filtered out — the disabled entry is what carries the
/// explanation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendAvailability {
    pub backend: &'static CliBackend,
    pub path: std::path::PathBuf,
    pub usable: bool,
}

/// Which backends the user actually has, given a binary resolver.
///
/// `resolve` is injected rather than calling a `which` helper directly: this
/// crate is consumed by the Hub, the runtime and the CLI, each of which
/// resolves binaries differently (the Hub must ask an interactive login shell,
/// because a Finder-launched app inherits a bare PATH). Injection also makes
/// every case below testable without touching the real PATH.
pub fn available<F>(resolve: F) -> Vec<BackendAvailability>
where
    F: Fn(&str) -> Option<std::path::PathBuf>,
{
    REGISTRY
        .iter()
        .filter_map(|b| {
            resolve(b.binary).map(|path| BackendAvailability {
                backend: b,
                path,
                usable: matches!(b.activation, Activation::Enabled),
            })
        })
        .collect()
}
```

- [ ] Add these tests inside the existing `mod tests` block in `mur-common/src/cli_backend.rs`:

```rust
    use std::path::PathBuf;

    fn found(_: &str) -> Option<PathBuf> {
        Some(PathBuf::from("/opt/homebrew/bin/claude"))
    }

    fn missing(_: &str) -> Option<PathBuf> {
        None
    }

    #[test]
    fn an_absent_binary_produces_no_entry() {
        // "Backends whose binary is absent do not appear in the UI at all."
        assert!(available(missing).is_empty());
    }

    #[test]
    fn a_present_binary_is_listed_with_the_path_that_was_resolved() {
        let got = available(found);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].backend.key, "claude");
        assert_eq!(got[0].path, PathBuf::from("/opt/homebrew/bin/claude"));
    }

    #[test]
    fn a_present_but_unverified_backend_is_listed_and_not_usable() {
        // The distinction this task exists for: absent is gone, unverified is
        // shown-and-disabled. Folding the second into the first would remove
        // the only surface that explains the unmet requirement.
        let got = available(found);
        assert!(!got[0].usable);
    }

    #[test]
    fn usable_tracks_activation_and_nothing_else() {
        // Guards against a future row being enabled by the mere fact that its
        // binary resolved.
        for a in available(found) {
            assert_eq!(
                a.usable,
                matches!(a.backend.activation, Activation::Enabled)
            );
        }
    }
```

- [ ] Run the tests and watch them pass:

```bash
cargo test -p mur-common cli_backend
```

Expected: `test result: ok. 10 passed; 0 failed`.

- [ ] Run lint and format:

```bash
cargo clippy -p mur-common -- -D warnings && cargo fmt --check
```

Expected: no output, exit 0.

- [ ] Commit: `git add mur-common/src/cli_backend.rs && git commit -m "feat(cli-backend): availability keeps a disabled backend visible"`

---

## Task 3 — The private CLI home

### Interfaces

**Consumes:** `CliBackend`, `CLAUDE` (Task 1).

**Produces:**
```rust
pub fn home_dir(mur_home: &std::path::Path, key: &str) -> std::path::PathBuf;
pub fn ensure_home(
    mur_home: &std::path::Path,
    b: &CliBackend,
) -> std::io::Result<(&'static str, std::path::PathBuf)>;
```

`ensure_home` returns the pair a caller sets on the child process: the env var name and the path it must point at.

### Steps

- [ ] Add to `mur-common/src/cli_backend.rs`, directly above the `#[cfg(test)]` module:

```rust
/// `<mur_home>/cli-homes/<key>/` — this backend's private CLI home.
///
/// `mur_home` is a parameter rather than a call to `trust::mur_home()` so the
/// path is a pure function of its inputs and every test runs against a temp
/// dir. Same shape as `local_llm::local_model_dir`.
pub fn home_dir(mur_home: &std::path::Path, key: &str) -> std::path::PathBuf {
    mur_home.join("cli-homes").join(key)
}

/// Create this backend's private home if absent and return the environment
/// variable that points the CLI at it.
///
/// The home starts empty and stays MUR's: the user authenticates once inside
/// it, and their own `~/.claude` / `~/.codex` is never read or written. We do
/// not copy `auth.json` — two holders of one refresh-token lineage each
/// rotating would log the user out of their own CLI, which is why the gateway
/// is the sole token holder on the other track.
pub fn ensure_home(
    mur_home: &std::path::Path,
    b: &CliBackend,
) -> std::io::Result<(&'static str, std::path::PathBuf)> {
    let dir = home_dir(mur_home, b.key);
    std::fs::create_dir_all(&dir)?;
    Ok((b.home_env_var, dir))
}
```

- [ ] Add these tests inside the existing `mod tests` block in `mur-common/src/cli_backend.rs`:

```rust
    #[test]
    fn home_dir_is_namespaced_under_cli_homes() {
        let got = home_dir(std::path::Path::new("/tmp/murhome"), "claude");
        assert_eq!(got, PathBuf::from("/tmp/murhome/cli-homes/claude"));
    }

    #[test]
    fn ensure_home_creates_the_dir_and_returns_the_env_var() {
        let tmp = std::env::temp_dir().join(format!("mur-cli-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let (var, dir) = ensure_home(&tmp, &CLAUDE).expect("create");
        assert_eq!(var, "CLAUDE_CONFIG_DIR");
        assert_eq!(dir, tmp.join("cli-homes").join("claude"));
        assert!(dir.is_dir());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn ensure_home_is_idempotent() {
        let tmp = std::env::temp_dir().join(format!("mur-cli-home-idem-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        ensure_home(&tmp, &CLAUDE).expect("first");
        let marker = home_dir(&tmp, "claude").join("settings.json");
        std::fs::write(&marker, b"{}").expect("write marker");
        ensure_home(&tmp, &CLAUDE).expect("second");
        assert_eq!(std::fs::read(&marker).expect("read marker"), b"{}");
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn ensure_home_never_touches_the_users_own_cli_config() {
        // The Global Constraint, asserted rather than assumed. A stand-in for
        // ~/.claude sits OUTSIDE the mur home; creating the private home must
        // leave its bytes untouched.
        let base = std::env::temp_dir().join(format!("mur-cli-iso-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let user_cfg = base.join("user-claude");
        std::fs::create_dir_all(&user_cfg).expect("user cfg");
        let cred = user_cfg.join(".credentials.json");
        std::fs::write(&cred, b"user-token").expect("seed");

        ensure_home(&base.join("murhome"), &CLAUDE).expect("create");

        assert_eq!(std::fs::read(&cred).expect("still there"), b"user-token");
        assert!(
            !base
                .join("murhome")
                .join("cli-homes")
                .join("claude")
                .join(".credentials.json")
                .exists()
        );
        std::fs::remove_dir_all(&base).ok();
    }
```

- [ ] Run the tests and watch them pass:

```bash
cargo test -p mur-common cli_backend
```

Expected: `test result: ok. 14 passed; 0 failed`.

- [ ] Run lint and format:

```bash
cargo clippy -p mur-common -- -D warnings && cargo fmt --check
```

Expected: no output, exit 0.

- [ ] Commit: `git add mur-common/src/cli_backend.rs && git commit -m "feat(cli-backend): private CLI home per backend"`

---

## Task 4 — The Hub's two-track selection

Implements the spec's selection table exactly. Pure model, no React and no Tauri, so every row is a test.

### Interfaces

**Consumes:** `HookState` from `mur-hub-gui/ui/src/components/chatgptSubscription.ts` (existing; `"true" | "false" | "unknown"`).

**Produces:**
```ts
export type CliGate = "absent" | "passed" | "failed";
export type GatewayOffer = "preferred" | "cta" | "none";
export type CliSpawnOffer = "offered" | "available" | "disabled" | "none";
export interface TrackOffer { gateway: GatewayOffer; cliSpawn: CliSpawnOffer }
export function trackOffer(hook: HookState, cli: CliGate): TrackOffer;
```

Vocabulary, so no consumer has to guess:

- `gateway: "preferred"` — the working track; `"cta"` — install / start button; `"none"` — not offered.
- `cliSpawn: "offered"` — the track to use; `"available"` — usable but not preselected; `"disabled"` — shown with its unmet requirements; `"none"` — no CLI, nothing to show.

### Steps

- [ ] Create `mur-hub-gui/ui/src/components/cliTrack.ts`:

```ts
/**
 * cliTrack.ts — which of the two tracks a vendor may offer.
 *
 * The spec's selection table, as a pure function. No React, no Tauri: the
 * table is the behaviour, so it is tested as a table.
 *
 * The rule that shapes every branch: `unknown` never disables a control and
 * never routes silently to CLI spawn. `false` is the gateway's own denial;
 * `unknown` only means we could not ask, and falling back costs a second
 * login and — for codex — an unmediated shell. Too much to spend on a
 * question we merely failed to ask, when the answer may be that the gateway
 * works.
 */

import type { HookState } from "./chatgptSubscription";

/** Whether the CLI is installed, and whether its safety probes passed. */
export type CliGate = "absent" | "passed" | "failed";

export type GatewayOffer = "preferred" | "cta" | "none";
export type CliSpawnOffer = "offered" | "available" | "disabled" | "none";

export interface TrackOffer {
  gateway: GatewayOffer;
  cliSpawn: CliSpawnOffer;
}

export function trackOffer(hook: HookState, cli: CliGate): TrackOffer {
  // The gateway works. Nothing else is needed, and the CLI track would only
  // charge a second login for the same capability.
  if (hook === "true") return { gateway: "preferred", cliSpawn: "none" };

  // Could not ask. Resolve that first — but never by removing a control.
  if (hook === "unknown") {
    if (cli === "passed") return { gateway: "cta", cliSpawn: "available" };
    if (cli === "failed") return { gateway: "cta", cliSpawn: "disabled" };
    return { gateway: "cta", cliSpawn: "none" };
  }

  // hook === "false": this build denied it, so the gateway CTA would be the
  // useless-repair loop. The CLI track is the only way through.
  if (cli === "passed") return { gateway: "none", cliSpawn: "offered" };
  if (cli === "failed") return { gateway: "none", cliSpawn: "disabled" };
  return { gateway: "none", cliSpawn: "none" };
}
```

- [ ] Create `mur-hub-gui/ui/src/components/cliTrack.test.ts`:

```ts
import { describe, it, expect } from "vitest";
import { trackOffer, type CliGate } from "./cliTrack";
import type { HookState } from "./chatgptSubscription";

describe("the selection table, row by row", () => {
  it("hook true: the gateway is the track, whatever the CLI is doing", () => {
    for (const cli of ["absent", "passed", "failed"] as CliGate[]) {
      expect(trackOffer("true", cli)).toEqual({ gateway: "preferred", cliSpawn: "none" });
    }
  });

  it("hook unknown, no CLI: gateway CTA", () => {
    expect(trackOffer("unknown", "absent")).toEqual({ gateway: "cta", cliSpawn: "none" });
  });

  it("hook unknown, CLI passed: CTA first, CLI available but not preselected", () => {
    expect(trackOffer("unknown", "passed")).toEqual({ gateway: "cta", cliSpawn: "available" });
  });

  it("hook unknown, CLI failed: CTA still live, CLI disabled", () => {
    expect(trackOffer("unknown", "failed")).toEqual({ gateway: "cta", cliSpawn: "disabled" });
  });

  it("hook false, CLI passed: CLI spawn is the track", () => {
    expect(trackOffer("false", "passed")).toEqual({ gateway: "none", cliSpawn: "offered" });
  });

  it("hook false, CLI failed: both off, the CLI panel names what is unmet", () => {
    expect(trackOffer("false", "failed")).toEqual({ gateway: "none", cliSpawn: "disabled" });
  });

  it("hook false, no CLI: neither track exists", () => {
    expect(trackOffer("false", "absent")).toEqual({ gateway: "none", cliSpawn: "none" });
  });
});

describe("the invariants the table exists to protect", () => {
  it("unknown never disables a control", () => {
    // Every unknown row keeps the gateway CTA live. This is the #1334 lesson:
    // the control that reflects availability is the button, not visibility.
    for (const cli of ["absent", "passed", "failed"] as CliGate[]) {
      expect(trackOffer("unknown", cli).gateway).toBe("cta");
    }
  });

  it("unknown never routes silently to CLI spawn", () => {
    // "available" is reachable; "offered" — the preselected track — is not.
    for (const cli of ["absent", "passed", "failed"] as CliGate[]) {
      expect(trackOffer("unknown", cli).cliSpawn).not.toBe("offered");
    }
  });

  it("unknown is never folded into false", () => {
    // If any CLI gate produced the same answer for both, the distinction the
    // whole tri-state exists for would have collapsed.
    for (const cli of ["absent", "passed", "failed"] as CliGate[]) {
      expect(trackOffer("unknown", cli)).not.toEqual(trackOffer("false", cli));
    }
  });

  it("every hook state answers for every CLI gate", () => {
    for (const hook of ["true", "false", "unknown"] as HookState[]) {
      for (const cli of ["absent", "passed", "failed"] as CliGate[]) {
        expect(trackOffer(hook, cli)).toBeTruthy();
      }
    }
  });
});
```

- [ ] Run the tests and watch them pass:

```bash
cd mur-hub-gui/ui && npx vitest run src/components/cliTrack.test.ts
```

Expected: `Test Files  1 passed (1)` and `Tests  11 passed (11)`.

- [ ] Typecheck and lint:

```bash
cd mur-hub-gui/ui && npx tsc -b && npx eslint src/components/cliTrack.ts src/components/cliTrack.test.ts
```

Expected: no output from either, exit 0.

- [ ] Commit: `git add mur-hub-gui/ui/src/components/cliTrack.ts mur-hub-gui/ui/src/components/cliTrack.test.ts && git commit -m "feat(hub): two-track selection model"`

---

## Done when

- [ ] `cargo test -p mur-common cli_backend` — 14 passed.
- [ ] `cd mur-hub-gui/ui && npx vitest run src/components/cliTrack.test.ts` — 11 passed.
- [ ] `cargo clippy --workspace -- -D warnings && cargo fmt --check` — clean.
- [ ] `claude` appears in `available()` on a machine that has it, with `usable: false`.
- [ ] No row in `REGISTRY` for `codex` or `agy`.

## Spec coverage

| Spec section | Covered by | Notes |
|---|---|---|
| Backend registry, not three hardcoded paths | Task 1 | `codex` / `agy` rows deferred to open questions 1–3 |
| Backends whose binary is absent do not appear | Task 2 | |
| Isolation: a private CLI home per backend | Task 3 | user-config isolation asserted |
| Activation gate: agy remains disabled | Tasks 1, 2 | enforced structurally — an unprobed backend has no row |
| Track selection and the Hub button | Task 4 | model only; rail wiring follows the spawn work |
| Chosen: CLI owns the loop, MUR owns the tools | **not covered** | needs an MCP server that serves the agent's tools; see "Out of scope" |
| Conditional boundary: codex keeps an unmediated shell | **not covered** | no codex row until its sandbox and envelope are verified |
| Token lineage verification | **not covered** | per-vendor probe, gates a backend's ship, not this slice |
