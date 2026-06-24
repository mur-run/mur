# Agent runtime upgrade: stale-detection + version-gating + graceful restart

- **Date:** 2026-06-24
- **Status:** Design (approved for plan)
- **Topic:** When a `mur-agent-runtime` upgrade lands on disk, get running agents onto the new binary safely — **detect** stale runtimes, **version-gate** the A2A wire so version skew fails with an actionable error (not opaque `-32601`), and **gracefully restart** agents on demand (drain in-flight work; never auto-bounce).

## Problem

Per-agent runtimes run as long-lived launchd-managed processes (`KeepAlive=true`). A package upgrade (brew, dev install, Hub `.app`) replaces `mur-agent-runtime` **on disk**, but a running agent keeps executing its **old** process until restarted — so it drifts onto a stale binary.

This bit us live:
1. A running agent (started after the #489 macOS DNS fix merged) was still executing a **pre-#489 binary**, so its sandbox blocked DNS and its outbound LLM call failed.
2. A freshly-built MCP client dialing `channel/delegate` against an **old runtime that never registered that method** failed with a bare JSON-RPC `-32601`, surfaced as a generic `"agent X returned error"` — undiagnosable.

The fix isn't "auto-restart everything on upgrade" — that silently kills in-flight agent turns (an autonomous-loop-safety violation; see `feedback_autonomous_loop_safety_audit`). Mature tools (`needrestart`, systemd) **detect and notify; restart deliberately; auto-restart only opt-in.**

## Goals

- **Detect** stale runtimes (a running agent's binary differs from the installed one) and surface it in `mur agent doctor` / `status` + a post-upgrade nudge. Print-only; never acts.
- **Version-gate** the A2A dial: refuse a versioned method against a peer that doesn't support it, with an actionable `run 'mur agent restart X'` error instead of `-32601`. Never silently downgrade.
- **Graceful restart**: `mur agent restart` drains the in-flight turn (bounded), then lets launchd respawn on the fresh binary. Fleet loops bail at the iteration boundary.

## Non-goals (deferred)

- **Auto-restart on upgrade** (`MUR_AGENT_AUTORESTART` opt-in env switch). Out of this spec by the user's scoping (① + ② + ③ only). When built, it mirrors `MUR_FLEET_AUTORUN`: OFF by default, post-transaction, via the graceful path.
- **In-place re-exec / hot state hand-off** (`systemctl daemon-reexec`-style serialize→re-exec→deserialize). Large lift; graceful drain + respawn suffices.
- **Multi-version A2A negotiation matrix** (running mixed-version fleets). Ship detect-and-refuse first.
- **`needrestart`-style per-file `/proc/maps` allow/denylist.** Our check is a single build-id compare, no spurious stale-file hits to suppress.

## Best-practice grounding (2026)

- **Detect always; restart never-by-default; auto-restart opt-in.** `needrestart` (Debian/Ubuntu default) defaults to list-only under automation, default answer "no", and excludes stateful/connectivity-critical services from auto-restart. systemd RFE #32099 deliberately separates *detection* from *automatic action*. → MUR notifies; never bounces a running agent.
- **Fail explicit on version skew at the wire; never silently mismatch.** A2A mandates a version handshake + `VersionNotSupportedError` over best-effort; RFC 9368 (QUIC) abandons cleanly when there's no common version. → the dial refuses with an actionable error, not `-32601`.
- **Restart cooperatively via graceful drain, not SIGKILL.** SIGTERM → stop new work → finish in-flight within a bounded grace → exit 0; treat SIGTERM exit as success. The OS won't drain for you. → the runtime must drain the in-flight turn.

## Key decision: the dial gates on a semantic proto-version, not the git-sha

The git-sha changes **every commit**, so gating the dial on it would refuse *protocol-compatible* peers needlessly. The dial gates on **`A2A_PROTO_VERSION`** — an integer (like `CHANNEL_SCHEMA_VERSION`) bumped **only on incompatible A2A method-surface changes**. The git-sha drives the **notify** (stale detection) only. Two signals, two jobs.

## Design

### 0. Shared infra — build-id + proto version

**Build-id (`mur-common`):**
- `mur-common/build.rs` runs `git rev-parse --short=12 HEAD`, emits `cargo:rustc-env=MUR_GIT_SHA=<sha>` (+ `cargo:rerun-if-changed=.git/HEAD` so a new commit forces a rebuild). Fallback to `"unknown"` when git is unavailable (crates.io publish / no `.git`).
- `mur-common/src/build.rs` (new module): `pub const SHORT_SHA: &str = env!("MUR_GIT_SHA");`. Both `mur-core` (CLI) and `mur-agent-runtime` read `mur_common::build::SHORT_SHA`.
- `mur-agent-runtime --build-id` prints `SHORT_SHA` (one-line, for the doctor compare).

**Proto version (`mur-common`):**
- `pub const A2A_PROTO_VERSION: u32 = 1;` — the current A2A method-surface version. Bump on any incompatible change to a dialed method.
- `pub fn method_min_proto(method: &str) -> u32` — the minimum proto a peer must advertise for a method. `"channel/delegate" => 1`; default `0` (unversioned/always-available methods like `message/send`).

**Carrier fields:**
- `LockFile` (`mur-common/src/agent.rs:835`) gains `#[serde(default)] pub build_sha: String` + `#[serde(default)] pub proto_version: u32`. Written at `supervisor.rs:507` from `mur_common::build::SHORT_SHA` and `A2A_PROTO_VERSION`. `#[serde(default)]` so an **old** lock (no field) reads as `""` / `0` — which is exactly "predates this feature → stale / unsupported".
- `AgentCard` (`protocol/methods/card.rs:60`) gains `"proto_version": A2A_PROTO_VERSION` alongside the existing `"protocolVersion": "a2a/0.3"` string.

### ① Dial version-gate (`mur-core/src/a2a_dial.rs`)

In `dial_method`, before sending a request for a versioned method:
1. Read the peer's advertised `proto_version` — from its `running.lock` (cheap, already on disk) — defaulting to `0` when absent.
2. If `peer_proto < method_min_proto(method)` → return a structured error (new `A2aDialError::StaleRuntime { agent, peer_proto, needed, build_sha, method }`) surfaced as:
   ```
   agent 'rustsmith' is running a stale runtime (proto 0, build unknown);
   the requested capability 'channel/delegate' needs proto 1.
   Run 'mur agent restart rustsmith' to apply the installed runtime.
   ```
3. Methods with `method_min_proto == 0` are never gated (back-compat: `message/send` etc. dial unchanged).

This is a **pre-flight** check (no wasted dial), the A2A `VersionNotSupportedError` pattern: refuse cleanly, never silently downgrade, never auto-restart the peer. Where the dial currently surfaces `-32601` generically (`a2a_dial.rs:154`), a registered-but-incompatible peer is now caught *before* the call; a genuinely-unknown method still surfaces its `-32601` (unchanged).

**One-time cutover (intended, not a bug):** every runtime that predates this feature writes no `proto_version` → reads as `0` → is gated for `channel/delegate` (min 1) until restarted, *including* ones that technically registered the method. This is correct: those runtimes are stale by definition (they also lack #489 etc.), and the gate converts today's silent/opaque failure into one actionable nudge — "upgrade, then `mur agent restart --stale`". After the first restart onto a proto-1 binary, the gate only fires again on a *future* incompatible bump. `method_min_proto == 0` methods (`message/send`, …) are never gated, so basic agent comms keep working through the cutover.

### ② Notify — `doctor` + `status` + post-upgrade nudge (print-only)

**Stale rule:** an agent is stale iff `running.lock.build_sha != <on-disk runtime>.build_sha`. The on-disk build-id comes from one exec of `resolve_runtime_target() --build-id` (the binary a restart would actually launch — the precise "would restart change anything?" question).

- **`mur agent doctor`** (new, `cmd/agent/doctor.rs`): for each agent, report `running` + `stale|current` + lock proto/build vs on-disk. Stale agents get the `→ run 'mur agent restart <name>'` hint. Exit non-zero if any stale (scriptable). Supports `--json`.
- **`mur agent status`** (`lifecycle.rs:462`): add a `stale runtime — restart to apply` marker to the existing per-agent line.
- **Post-upgrade nudge:** `install.sh` (after it replaces the binary) and a one-shot check on the next `mur` invocation print: `N agent(s) are running a stale runtime. Run 'mur agent restart --stale' (--dry-run to list).` Print-only, like `needrestart -r l` — never prompts-to-restart.

### ③ Graceful restart — runtime drain + new command

**Runtime drain (the one runtime behavior change, `supervisor.rs:648-672`, closes the `:655` TODO):**
On SIGTERM, before aborting transports: tell the `TaskRunner` to **stop accepting new dials** (new `channel/delegate`/`message/send` get a transient `"agent draining, retry"` error) and **await the in-flight turn**, bounded by `lifecycle.stop_timeout_secs`. On timeout, proceed with teardown (don't hang forever). Then the existing teardown (hooks → transports → MCP pool → flush → remove lock). SIGTERM-driven exit stays exit 0 (not a crash).

**`mur agent restart` (new, `cmd/agent/restart.rs`):** `mur agent restart <name> | --stale | --all [--dry-run]`.
- Resolve targets: a named agent, `--stale` (all stale per ②), or `--all` (all running).
- For each: read pid from `running.lock`, send `SIGTERM` (graceful drain above). launchd `KeepAlive=true` respawns the agent on the **fresh** on-disk binary; poll the new socket/lock for readiness (bounded), report old→new build-id.
- `--dry-run`: list what would be restarted (and why), act on nothing.
- **`--stale` is the upgrade-apply command** (only restarts agents where the binary would actually change → least disruption); `--all` is the blunt force-all. The nudge points at `--stale`.
- Fleet `--loop` iterations already bail at the boundary via `LoopStop` (`loop_run.rs`) — no mid-loop kill; reuse as-is.

## Error handling

- Dial gate: `StaleRuntime` is a typed variant with a human-actionable `Display`; callers (workflow executor, fleet, MCP `parallel_jobs`) surface it verbatim. It does **not** abort sibling steps that target *supported* peers.
- `build.rs` git failure → `SHORT_SHA = "unknown"`; stale-detection treats `"unknown" != "unknown"`? No — two `"unknown"`s compare equal → "current" (no false-stale spam on git-less installs). Proto-gate still works (proto_version is independent of git).
- Restart: a missing/again-stale `running.lock`, a pid that's already gone, or a respawn that doesn't come up within the poll window → reported per-agent, never silently swallowed; `--all`/`--stale` continue past one failure.

## Testing

- **Pure unit:** `method_min_proto` mapping; the stale predicate (`build_sha` compare incl. the `"unknown"==... ` case); `proto_version` serde default (old lock → 0); the dial-gate decision (`peer_proto < needed` → `StaleRuntime`, `== 0 min` → pass).
- **Dial gate:** a lock with `proto_version: 0` + a `channel/delegate` dial → `StaleRuntime` error naming the agent + restart hint; `proto_version: 1` → proceeds.
- **Drain:** runtime under SIGTERM with a stub in-flight task → awaits it (up to `stop_timeout_secs`) before teardown; with no task → exits promptly; over-timeout → tears down anyway.
- **doctor/restart:** against a temp `MUR_HOME` with synthetic `running.lock`s (one stale, one current) → doctor lists exactly the stale one; `restart --dry-run --stale` names exactly it.
- **Live (operator):** restart a real running agent → it drains, launchd respawns on the fresh binary, doctor flips it `stale→current`, and the `channel/delegate` dial that previously errored now succeeds.

## File touch list

- `mur-common/build.rs` (new) — embed `MUR_GIT_SHA`.
- `mur-common/src/build.rs` (new) + `lib.rs` — `SHORT_SHA`, `A2A_PROTO_VERSION`, `method_min_proto`.
- `mur-common/src/agent.rs` — `LockFile.build_sha` + `proto_version`.
- `mur-agent-runtime/src/supervisor.rs` — write the new lock fields; `--build-id` flag; SIGTERM drain.
- `mur-agent-runtime/src/protocol/methods/card.rs` — advertise `proto_version`.
- `mur-agent-runtime/src/task_runner.rs` (or wherever the run loop lives) — `drain(timeout)`: stop-accepting + await active turn.
- `mur-core/src/a2a_dial.rs` — pre-flight proto-gate + `StaleRuntime` error.
- `mur-core/src/cmd/agent/{doctor.rs,restart.rs}` (new) + `mod.rs`/CLI wiring; `lifecycle.rs` status marker.
- `install.sh` — post-install stale nudge.

## Build order (one plan, by value)

1. **Shared infra (0)** — build-id, proto constant, lock/card fields. Foundation for everything.
2. **① Dial gate** — highest value; turns the opaque `-32601` into an actionable error immediately, even before any restart.
3. **③ Graceful drain + `restart`** — so the actionable error has a safe action to point at.
4. **② Notify** — `doctor`/`status`/nudge, which lean on (0) and point at (③).
