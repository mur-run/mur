# B0 Text-Only Safety Rules

The mur agent runtime enforces a 22-rule consumer-safe baseline (B0).
Rules 13-22 cover multimodal inputs (drag/drop, character cards) — see
[drag-drop-pipeline.md](drag-drop-pipeline.md) and
[character-cards.md](character-cards.md). Rules 1-12 cover text and
tool boundaries; this page documents the 7 in-hook text rules that
ship in v1.

| # | Rule                                                       | Where it fires            |
|---|------------------------------------------------------------|---------------------------|
| 1 | FS read-write confined to `~/.mur/agents/<name>/`           | `pre_tool_use` (advisory) |
| 2 | Outbound network allowlist + first-use AskUser + remember   | `pre_tool_use`            |
| 3 | Tool-result spotlighting (`<untrusted_tool_result>`)        | `on_prompt_submit`        |
| 4 | No same-turn tool chaining after fresh untrusted input ✓ M3.8 | `pre_tool_use`          |
| 5 | Shell / `eval` / spawn deny by default                      | `pre_tool_use`            |
| 7 | Outbound secret pre-filter (regex over body)                | `on_message_send`         |
| 8 | Memory-write PII redaction                                  | `post_tool_use`           |
| 11| MCP binary signature check (macOS/Windows)                  | `on_startup`              |

Rules 6 (MCP install hash pinning), 9 (telemetry redaction), 10 (UX
tier description), and 12 (companion proactive default-quiet audit)
ship in companion plans — they are out-of-hook concerns (CLI verb /
tracing layer / UX architecture / pre-existing in M2.x).

## Pipeline

The order of operations on a single tool call:

1. `pre_tool_use` runs in this order, returning the FIRST hit:
   1. Rule 1 — fs.write/delete/append/create outside agent_home → AskUser
   2. Rule 5 — shell/spawn family + spawn.mode=Allowlist → Deny if argv[0] not in allowed[]
   3. Rule 2 — network.* + Restricted mode + host not in allow_hosts → AskUser (after GrantStore lookup)
   4. Rule 4 — `after_untrusted_input` flag set + side-effect tool → AskUser (M3.8)
2. `post_tool_use` runs Rule 8 if the call's name starts with `memory.`.
3. `on_message_send` runs Rule 7 over `body`.
4. `on_prompt_submit` runs Rule 3 (wrap prior tool messages) and the
   M3.8 untrusted-input spotlighting branch.
5. `on_startup` runs Rule 11.

## How to extend

To add a new B0 rule:

1. Add the spec text to `docs/superpowers/specs/2026-04-30-mur-agent-harness-roadmap-design.md` §6.1.
2. Add a pure helper in `mur-agent-runtime/src/hooks/b0_helpers.rs`
   if the rule has logic worth testing in isolation.
3. Add a branch inside the appropriate `B0SafetyHook` async method in
   `mur-agent-runtime/src/hooks/b0.rs`.
4. Add `mur-agent-runtime/tests/b0_rule<N>_<short_name>.rs` with at
   least one positive + one negative case.
5. Add the test name to `scripts/e2e/v1-b0-text-rules.sh` so it runs
   in the smoke suite.

## Acceptance

- 7/7 rule test files pass on the host CI matrix (macOS + Linux + Windows).
- `scripts/e2e/v1-b0-text-rules.sh` exits 0.
- M3.8 (Rule 4) and M3.x multimodal rules (13-22) are unchanged by M7.

## What B0 does NOT do

B0 is the v1 consumer-safe baseline — best-effort defense in depth, not
a real sandbox. Hard runtime confinement (Landlock on Linux, App
Sandbox / SBPL on macOS, AppContainer on Windows) lives in B1
(`docs/superpowers/specs/...` §6.3) and ships in v2. Until B1, treat
B0 as a robust prompt-injection guard + obvious-foot-gun blocker, not
a malware containment surface.
