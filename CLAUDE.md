# CLAUDE.md

Operational guidance for Claude Code working in this repository. Anything a task
does not need is in `docs/architecture/runtime-overview.md` — read it only when
the task touches that subsystem.

## Lint

```bash
# CI's own invocation; --all-targets is load-bearing
cargo clippy --all --all-targets --no-deps --locked -- -D warnings
cargo fmt --all -- --check
```

## Architecture

Cargo workspace of small crates plus two workspace-excluded Tauri apps. The load-bearing ones:

- **`mur-common`** — Shared types, plus the small persisted files those types own (`Config`, `AgentProfile`, `ModelRegistry`, `LockFile`, ledger). It does do file I/O; what it does not hold is pipeline or CLI logic.
- **`mur-core`** — All CLI logic and the `mur` binary. Modules map to the four-stage memory pipeline (`capture/ → store/ → retrieve/ → inject/`, with `evolve/` alongside). Hosts `mur agent ...`.
- **`mur-agent-runtime`** — Per-agent A2A v0.3 supervisor (P0a). One binary, one BusyBox-style symlink per agent (`mur_agent_<name>` → `mur-agent-runtime`). Crate README has the walkthrough.
- **`mur-daemon`** — Long-running background daemon binary.
- **`mur-mcp-proto`** — JSON-RPC 2.0 types and stdio framing for MCP. Protocol only, no dispatch loop: `mur-mcp-server` answers requests while the runtime's shim must also *originate* them (`elicitation/create`, the HITL transport), so the two loops are different shapes.
- **`mur-mcp-server`** — MCP server binary (stdio JSON-RPC). Read-only; mutations go through hooks.
- **`mur-gui-core`** — Shared GUI library (sidecar supervisor, companion bridge, A2A client). Consumed by `mur-hub-gui` and during migration also by `mur-agent-gui`.

**Shared state with its own file format gets its own crate** — `mur-channel`, `mur-compress`, `mur-open-items`, `mur-mcp-proto`. The rule exists because `mur-agent-runtime` must not depend on `mur-core` (that pulls LanceDB + Arrow into every agent process), so anything both of them read or write has to live below both. Reach for this before adding I/O to `mur-common`.

Workspace-excluded Tauri 2 GUI apps (built via their own manifests so `cargo build --workspace` does not pull WebKitGTK / Cocoa / WebView2):

- **`mur-agent-gui`** — Per-agent `.app` shell (legacy; deprecated in M-h8).
- **`mur-hub-gui`** — MUR Hub cross-agent desktop app (in development; replaces `mur-agent-gui` in v1).

Layers: Agent Runtime (P0a) · Memory/Learning (four-stage pipeline) · Agent
Infrastructure (MCP server, skills, action pipeline, cost router) · Human
Interface (Companion, Hub GUI, Slack bridge) · Governance (Commander).

Skills are the primary knowledge object (`mur-common/src/skill/`); all runtime
data lives under `~/.mur/`. Details, tier half-lives, and the on-disk layout:
runtime-overview.md.

## CLI Surface (top level)

One line each. Full flags, safety rules, and rationale are in
`docs/architecture/runtime-overview.md` — read the matching section before
changing any of these.

- `mur verify [--file path] [--all]` — scan docs for stale claims (paths, commands, code refs).
- `mur agent <sub>` — create / list / status / send / card / dial / cli / export / doctor / prompt / mcp / skill / perm / secret / companion / rekey / schedule. `murmur` is the quick form of `mur agent cli`; bare `murmur` opens the concierge. `dial` is the passthrough escape hatch for A2A methods without a wrapper.
- `mur fleet {create|list|show|status|run|stop|start|export|import|partition-plan|merge}` — squads over one signed channel. **Safety triad, do not weaken:** unattended autorun OFF unless `MUR_FLEET_AUTORUN=1`; autorun requires resolvable `limits:`; `mur fleet stop` is the kill-switch. Unattended HITL **defers, never times out**, matched on `action_hash`.
- `mur limits <name>` / `mur fleet limits` / `mur agent limits` — the three execution knobs per scope (deadline / stuck / cost_usd). Iteration caps and token budgets are gone.
- `mur model {connect|import|add|list|show|remove|migrate|prices|role|doctor}` — `~/.mur/models.yaml` registry. `doctor` is offline, read-only, warn-only, and never rewrites a model id.
- `mur monitor {add|list|show|cancel|retry}` — durable monitors for async work. `unknown` is never reported as `failed`; above-`read`-tier actions park a pinned approval.
- `mur official {list|install <id>}` — official catalog; installs carry an account-bound license.
- `mur deep-research {setup|""} [question]` — web research. Egress consent is explicit (`--grant-egress`).

Before changing anything that pins, verifies, or launches an MCP server, read `docs/architecture/mcp-supply-chain.md` — it records what each check covers, what it structurally cannot, and the two things deliberately not built (with reasons, so they don't get re-proposed).

## Development Notes

- Rust edition 2024 — `let` chains stable (`if let … && let …`)
- `Pattern` implements `Deref<Target = KnowledgeBase>` — access fields directly
- YAML writes use temp file + rename for atomicity (`store/yaml.rs`)
- `tracing` for structured logging; enable with `RUST_LOG=debug`
- Plans live in `docs/superpowers/plans/`. OpenSpec change specs in `openspec/changes/`.
- Unified Channel v3a–v4a (signed events, HITL gate, mobile sync): runtime-overview.md.

## Release Process

`main` is protected, so the whole release is one PR: **merging the version bump to `main` IS the release.** `tag.yml` tags the merge commit and dispatches `release.yml` automatically — there is no manual tag step, so review the bump PR as the release approval. Full step-by-step: the **`mur-release`** skill (`.claude/skills/mur-release/`).

## Documentation Checklist

After a user-facing change, update all three: **`README.md`**, the **docs site** (https://app.mur.run/docs/core), and the **product page** (https://app.mur.run/products/mur). Exact source paths and publish gotchas live in the **`update-docs`** skill — use it, don't reconstruct the paths.

## Mandatory Rules

1. **Design skills for every MUR install, not just this machine.** No local paths, no this-user's home directory, no this-repo-only assumptions baked into a skill's logic or examples — a skill installed on someone else's machine must work unmodified. Generalize before writing.
2. **No hardcoded values.** Use constants, config, or env vars. Research best practice if unsure.
3. **Ask, don't guess.** If requirements / paths / API contracts / behavior are ambiguous, ask. In auto mode, make low-risk assumptions and flag them.
4. **SSH connection.** Use Desktop Commander to ssh, not Bash/SSH.
5. **Single source file ≤ 800 lines.** When approaching the limit, split into submodules following the same structural pattern as siblings. Pure code movement first; behavior changes in a separate PR.
6. **Read narrowly.** Prefer LSP queries (goToDefinition, findReferences) and `grep`/`Grep` over reading whole large files. When you must read a file, target the relevant range with `offset`/`limit`.
7. **CLAUDE.md is operational, not a changelog.** Historical milestone descriptions, completed phase notes, and detailed design walkthroughs belong in `docs/architecture/` or `docs/superpowers/specs/`. Keep this file lean so every session starts cheap.
8. **Brand name is uppercase "MUR".** Everywhere a user can see it — GUI strings, `display_name`, docs, marketing copy, companion/voice text, notifications. The ONLY exceptions are the CLI binary/command (`mur`), code identifiers, file paths, internal `name`/directory slugs, and the `~/.mur` home. Use `display_name` for the uppercase label; keep internal `name` lowercase so it matches the on-disk directory (the runtime spoof check is exact-match).
9. **Agent name lookup is case-insensitive (CLI).** `mur agent send mur` and `... Mur` must both resolve, via `a2a_dial::canonicalize_agent_name`; downstream uses the exact canonical name so the spoof check passes.

## Token Saving

Skip brainstorming and multi-step planning unless asked. Don't create plan files
automatically. Keep responses concise; don't restate requirements. Prefer direct,
one-pass implementation. No subagents for simple tasks.
