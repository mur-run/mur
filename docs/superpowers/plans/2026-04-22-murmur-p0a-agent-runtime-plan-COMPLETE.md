# Murmur P0a — Implementation complete

Branch: `feat/murmur-p0a` (worktree at `~/Projects/mur-murmur-p0a/`).
Spec: `docs/superpowers/specs/2026-04-22-murmur-p0a-agent-runtime-design.md`.
Plan: `2026-04-22-murmur-p0a-agent-runtime-plan.md` (+ `-part2.md`).
Coverage map: `2026-04-22-murmur-p0a-e2e-coverage.md`.

## What landed (Tasks 0 → 40)

### Foundation — Tasks 0–6
- new `mur-agent-runtime` crate scaffolded into the workspace
- shared types (`AgentProfile`, A2A envelopes, `LockFile`, telemetry constants) added to `mur-common`
- profile loader + `{{agent_home}}` expansion + sha256 digest + UUIDv7 validation
- entitlement-warning detector + category presets (research / commerce / monitor / notify / automation / custom)
- BusyBox-style `multi_call::extract_profile_name` with Windows `.exe` + spoof defense
- `running.lock` with flock + stale detection (handles BSD vs Linux flock semantics)
- macOS 104-byte `sun_path` fallback (bind in `/tmp`, symlink back to canonical)

### Telemetry + protocol — Tasks 7–13
- async telemetry writer: daily JSONL files + JSON-RPC notification fan-out (OTel GenAI + `mur.*`)
- JSON-RPC 2.0 dispatcher with full A2A error-code mapping (-32700 / -32600 / -32601 / -32602 / -32603 / -32000 / -32001 / -32002 / -32010 / -32011)
- `agent/card`, `message/send`, `tasks/get`, `tasks/cancel`, `tasks/list` handlers
- `TaskRunner` state machine (Submitted → Working → Completed/Failed/Cancelled) with sync + async paths and oneshot cancellation
- stdio + Unix socket transports (newline-delimited JSON-RPC, shared `Arc<Dispatcher>`, broadcast notifications, SO_PEERCRED / LOCAL_PEERCRED peer resolution)

### LLM, retry, comms policy — Tasks 14–17
- MCP client: stdio subprocess + `initialize` + `tools/list` + `tools/call`
- `LlmClient` trait + Ollama HTTP provider (token usage + 429 → `RateLimit`)
- retry policy executor with fixed / linear / exponential backoff + classifier
- `sends_to` (sender intent) + `accepts_from` (receiver-authoritative) glob matchers + `resolve_caller_name` walker

### Supervisor — Tasks 18–21
- full startup sequence: profile → telemetry → lock → dispatcher → transports → SIGTERM/SIGINT
- graceful shutdown: warning event, telemetry flush, transport abort, lock removal
- `task/progress` notifications around `message/send`
- Ollama backend wired into `TaskRunner` with `Event::LlmCall` telemetry emission

### CLI surface — Tasks 22–30
- `mur agent create / list / status / stop / remove --purge / rename`
- `mur agent send <name> <message-json>` + `mur agent card <name>` (now with ephemeral runtime fallback in Task 38)
- `mur agent install-service` (launchd plist on macOS, systemd `--user` unit on Linux)
- `mur agent prompt {show|edit|set [-f file]}` with `.bak` preservation
- `mur agent mcp {add|list|remove|rename}` with spawn-allowlist sync
- `mur agent skill {add|list|remove|show}` with backing-file copy + orphan cleanup
- `mur agent perm` — full entitlement editing (`set-mode`, `allow/deny-host`, `list-hosts`, `allow-read/write/spawn`, `deny-path/spawn`, `set-limit`, `show`)

### YAML editing + packaging — Tasks 31–36
- `mur_core::yaml_edit` lib module: atomic write + comment-preserving top-level scalar set
- `.murpkg` exporter (sanitises notification secrets + socket auth, generates README + manifest + SHA-keyed identity)
- `.murpkg` importer (fresh UUIDv7, re-pointed socket bind, `--as` rename, missing-prereq report)
- self-contained binary feature: `embedded-agent` + `build.rs` tar/gzip + `include_bytes!`
- first-run extraction to `~/.cache/murmur/<digest12>/` with idempotency marker, supervisor divert + `MUR_AGENT_EXTERNAL_PROFILE` override
- `mur agent export --format=pkg|bin` driving cargo build for the bin path
- `prereq_check::check_mcp_prereqs` + `format_missing_error` for spec §11.3 startup failure

### Polish — Tasks 37–40
- `mur agent stats` (telemetry aggregation: llm_calls, tokens, avg latency, errors)
- `mur agent logs --tail N` (stderr.log tail; --follow deferred)
- `mur agent card` ephemeral fallback (cold-start runtime to fetch card)
- `scripts/e2e/run-all.sh` aggregating workspace tests + `--ignored` E2E + optional llvm-cov
- crate README, top-level README mention, CLAUDE.md Agent Runtime section, this completion log

## Numbers

- 41 commits on `feat/murmur-p0a` (Tasks 0–40 + this docs commit).
- 17 integration test files in `mur-agent-runtime/tests/` and 9 in `mur-core/tests/agent_*.rs`, plus several unit tests inline.
- 0 clippy warnings under `-D warnings` for both crates with default features and with `embedded-agent` enabled.
- All workspace tests + `--ignored` pass via `scripts/e2e/run-all.sh`.

## Plan deviations & rationale

Each commit message documents the deviation it introduces. The
load-bearing ones:

- **Transport sharing.** Plan called for `dispatcher.clone_structure()`; we wrap the entire `Dispatcher` in `Arc` and pass `Arc<Dispatcher>` to both `serve_stdio` and `serve_unix`. Same effect, fewer moving parts.
- **macOS flock semantics.** `is_stale` short-circuits when `lock.pid == std::process::id()` because BSD `flock(2)` allows reacquiring from a second OFD in the same process; without the short-circuit `live_lock_not_stale` failed on macOS.
- **Comment preservation in YAML.** Plan suggested `serde_yaml_ng::with_comments` (no such API). Implemented a line-aware text mutator for top-level scalars; nested edits go through the typed editor and accept comment loss.
- **Mock MCP server crate.** Plan made `mock_mcp` a separate workspace crate; `CARGO_BIN_EXE_<name>` is only injected for binaries in the same package, so it's now a `[[bin]]` of `mur-agent-runtime` (`test = false`).
- **profile_stdio.yaml fixture** disables the unix socket so the supervisor integration test can't race on `/tmp/agent.sock` between concurrent runs.
- **Slow `--format=bin` e2e** is `#[ignore]`'d (drives full `cargo build --features=embedded-agent`); opt-in via `cargo test -- --ignored` or `scripts/e2e/run-all.sh`.
- **Coverage gate.** `cargo-llvm-cov` is opt-in via `--coverage` flag (not pinned as workspace dev-dep, not wired into CI in this branch).

## Final verification (per plan)

| Check | Status |
|-------|--------|
| `cargo build --workspace --release` | green (run by `scripts/e2e/run-all.sh`) |
| `cargo test --workspace --all-targets` | green |
| `cargo test --workspace --all-targets -- --ignored` | green |
| `cargo clippy --workspace -- -D warnings` | green |
| `cargo fmt --check` (touched files) | green |
| `cargo llvm-cov --workspace --fail-under-lines 85` | opt-in (`--coverage`) |
| `scripts/e2e/run-all.sh` | green |

## What's deferred to P0b / P1

- TaskRunner active-task cancellation list (`SIGTERM` currently aborts transports but does not call `runner.cancel(...)` on each in-flight task).
- supervisor side: invoking `prereq_check::check_mcp_prereqs` against the bundled manifest on embedded-mode startup (the helper is library-ready).
- `mur agent logs --follow` streaming (notify watcher).
- CI workflow file (`.github/workflows/ci.yml`) wiring up the smoke runner + coverage gate.
- E2E test for the slow `--format=bin` build path (currently `#[ignore]`'d; flip when CI has caching).
