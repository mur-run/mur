# mur Hooks Redesign — Progressive Disclosure + Adaptive Gate + murmurd

**Status:** Design (approved 2026-05-04, no backward compatibility — pre-launch)
**Owner:** alan@twdd.com.tw
**Implements:** complete rewrite of `inject` / `capture` / `evolve` hook surface

---

## 1. Problem statement

The current hook system (`~/.mur/hooks/on-prompt.sh`, `on-tool.sh`, `on-stop.sh`, installed by `mur init --hooks`) has four documented failures:

1. **No progressive disclosure.** Every triggered injection dumps up to 5 patterns × ~2000 tokens, regardless of conversation complexity. Token budget is fixed in `mur-core/src/inject/hook.rs`. There is no metadata-only / snippet / full-body tier separation.
2. **No intent awareness.** `inject::hook::detect_trigger()` only inspects keywords (`error`/`retry`/`fail`/`錯誤`/`失敗`/`還是不行`). Greetings, acknowledgements (`ok` / `符合` / `好的`), meta commands, and pure conversation all trigger the same full retrieval pipeline.
3. **Synchronous prompt-time injection.** `on-prompt.sh` runs `mur context --compact` inline before the prompt continues. On cold cache or with embedding lookup, this blocks the user-facing turn for hundreds of milliseconds. Only `on-stop.sh` uses background execution (`(...) &`), and even that is fragile (parent shell can `wait` indirectly through the hook executor).
4. **Inconsistent cross-tool implementation.** Nine tool integrations (Claude Code, Auggie, Gemini CLI, Copilot CLI, OpenClaw, Cursor, Codex CLI, OpenCode, Amp) plus six file-based tools, with different schemas, different event names, different timeout semantics, no shared normalisation layer.

## 2. Industry research (2025-2026)

Four design currents converge on the same architecture:

1. **Anthropic `async: true` / `asyncRewake: true`** (Claude Code, Jan 2026). Native background hook execution; `asyncRewake` wakes Claude with a system reminder on exit code 2. Plain `&` shell forks are obsolete for Claude Code.
2. **Agent Skills three-stage progressive disclosure** (Anthropic Dec 2025, adopted within weeks by OpenAI, Google, GitHub, Cursor). Layer 1 is `name` + `description` only (~80 tokens / skill, median); layer 2 loads full SKILL.md when relevant; layer 3 loads supporting files only during execution.
3. **Async queue worker pattern** (claude-mem, Mem0, Letta). Hooks must complete in < 1 second; AI compression / extraction takes 5-30 seconds; bridge is a JSONL queue plus a daemon worker. Hooks `enqueue → exit`. Worker reads queue, runs heavy work asynchronously, writes results back.
4. **FLARE-style adaptive retrieval** (Jiang et al. 2023, plus the 2025 calibration follow-ups). Retrieval is triggered by signals (low-confidence tokens in the original work; we substitute tool-call signals), not by every prompt. Skip retrieval entirely for confident / trivial generations.

Sources: [Hooks reference - Claude Code Docs](https://code.claude.com/docs/en/hooks), [Equipping agents for the real world with Agent Skills - Anthropic](https://www.anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills), [Agent Skills: Progressive Disclosure as a System Design Pattern](https://www.newsletter.swirlai.com/p/agent-skills-progressive-disclosure), [Hooks architecture - Claude-Mem](https://docs.claude-mem.ai/hooks-architecture), [Active Retrieval Augmented Generation (FLARE)](https://arxiv.org/abs/2305.06983), [Agentic RAG: A Survey](https://arxiv.org/abs/2501.09136), [Letta / MemGPT](https://docs.letta.com/concepts/memgpt/).

## 3. Architecture overview

```
┌─────────────────────────────────────────────────────────────┐
│ AI Tool (Claude Code / Gemini / Codex / Cursor / Auggie...) │
└─────────────┬─────────────────────────────────┬─────────────┘
              │ event                           │ event
              ▼                                 ▼
   ┌──────────────────────┐         ┌──────────────────────┐
   │ on-prompt.sh         │         │ on-tool.sh           │
   │ (UserPromptSubmit)   │         │ (PreToolUse /        │
   │ — exec mur hook prompt│        │  PostToolUse)        │
   └──────────┬───────────┘         └──────────┬───────────┘
              │ stdin → stdout JSON            │ enqueue only
              ▼                                ▼
   ┌──────────────────────────────────────────────────────┐
   │ mur hook <event> ── unified entry (Rust binary)      │
   │   1. write NDJSON event to ~/.mur/queue/events.jsonl │
   │   2. for prompt: read inbox, return additionalContext│
   │   3. exit < 50ms (no LLM, no vector search)          │
   └──────────────────────┬──────────────────────┬────────┘
                          │                      │
                          ▼                      ▼
              ┌────────────────────┐   ┌──────────────────────┐
              │ Adaptive Gate      │   │ murmurd (daemon)     │
              │ (local, <5ms)      │   │ — Tokio worker pool  │
              │ — intent + tools + │   │ — notify watcher     │
              │   prefetch cache   │   │ — retrieve / extract │
              │ → Skip/L0/L1/L2    │   │   / emerge / evolve  │
              │                    │   │ — writes inbox/*.md  │
              └────────────────────┘   └──────────────────────┘
```

Three core mechanisms:

- **`mur hook` is the only entry point.** All nine tools call `exec mur hook <event> --tool <name>`. Tool-specific stdin schemas are normalised inside the Rust binary.
- **`murmurd` is a long-running daemon** (new crate `mur-daemon/`). It owns all heavy work: vector retrieval, LLM-based extraction, emergence detection, decay/maturity sweeps. It writes results to `~/.mur/inbox/<session>.md` so the next hook reads pre-computed context.
- **Adaptive Gate replaces keyword detection.** A pure-rule local classifier (intent regex + tool-call history + prefetch cache hit rate) returns `Skip`, `L0`, `L1`, or `L2 prefetch` in < 5ms.

## 4. Three-tier progressive disclosure

| Tier      | Token budget | When injected                          | What is injected                                     |
|-----------|--------------|----------------------------------------|------------------------------------------------------|
| Skip      | 0            | gate score < 0.3                       | nothing                                              |
| L0 Index  | 150-300      | SessionStart only — once per session   | capability metadata: `name — description` × N        |
| L1 Snippet| ~500         | UserPromptSubmit, gate ∈ [0.5, 0.8)    | 1-3 patterns: description + first technical para     |
| L2 Body   | 1500-2000    | PreToolUse(`Edit`/`Write`/`Bash`)      | full pattern body + principle + linked workflows     |

L0 example (one capability index, ~600 tokens for ~20 entries):

```markdown
## mur learning index (project: mur)
- `tokio-async-runtime` — Tokio: spawn / select! / time::sleep
- `clap-derive-cli` — Clap derive API for CLI parsing
- `thiserror-typed-errors` — typed error enums via #[derive(thiserror::Error)]
- `anyhow-context-chains` — .with_context() over .unwrap()
...
Run `mur recall <name>` to load full content of any item above.
```

L1 / L2 selection is performed by `murmurd` in advance; `mur hook prompt` only reads `~/.mur/inbox/<session>.md`. If the inbox is stale (TTL 5 min), `mur hook prompt` requests a synchronous quick-path retrieval (gate cap = L1 only — no LLM-grade work in the hot path).

L2 attaches via Anthropic's `additionalContext` field (silent, not shown in transcript) on platforms that support it; on others it inlines as the script's stdout.

## 5. Adaptive Gate scoring

Composite score (`< 5ms` total, no LLM):

```
score = 0.30·intent + 0.25·tool_signal + 0.20·query_quality
      + 0.15·session_state + 0.10·prefetch_hit
```

**Intent (regex + length, first-match-wins):**

| Pattern                                              | Score |
|------------------------------------------------------|-------|
| pure ack / chitchat (`^(ok\|好\|thanks\|對\|嗯).{0,4}$`) | 0.0   |
| meta command (`^/(help\|status\|model\|clear\|...)`)   | 0.0   |
| pure question (`^(why\|what is\|為什麼\|解釋一下)`)        | 0.3   |
| contains file path or code identifier                | 0.7   |
| action verb (`修\|實作\|refactor\|build\|fix\|add\|test`) | 0.8   |
| > 80 chars + ≥ 2 tech terms                          | 0.9   |
| fallback                                             | 0.5   |

**Tool signal** (read last 5 tool calls from `~/.mur/session/active.json`):

| Recent tool history                                  | Score |
|------------------------------------------------------|-------|
| no tool calls in last 5 turns                         | 0.1   |
| only `Read` / `Glob` / `Grep`                         | 0.4   |
| `Bash` with build/test/lint command                   | 0.8   |
| `Edit` / `Write` / `NotebookEdit` present             | 0.9   |
| any `mcp__*` tool                                     | 0.7   |

When the event is **PreToolUse** (not UserPromptSubmit), `tool_name` is read directly from `tool_input` and tool_signal jumps to 0.9-1.0, bypassing intent.

**Query quality** = output of existing `noise_filter::filter()`. Pass → 1.0; fail (TooShort / Greeting / SingleWord / EmojiOnly / ShortCjk / Boilerplate) → 0.0.

**Session state**:

| Condition                                            | Score |
|------------------------------------------------------|-------|
| session age < 30s                                    | 0.7   |
| recent Edit/Write within last 60s                    | 0.9   |
| session age > 30 min and no tool activity            | 0.3   |
| fallback                                             | 0.5   |

**Prefetch hit** = cosine similarity between current query and inbox-cached query × 0.3 + recency_boost × 0.7. Penalises forcing a refresh when stale content already covers the topic.

**Score → tier mapping:**

| Score range | Tier     |
|-------------|----------|
| < 0.3       | Skip     |
| 0.3 – 0.5   | L0 only  |
| 0.5 – 0.8   | L1       |
| ≥ 0.8       | L1 + L2 prefetch into inbox (L2 only emits on PreToolUse) |

Workflow trigger detection (query contains workflow `name`/`alias`) bypasses the gate entirely and forces L1 inclusion of the workflow.

## 6. Cross-tool unified protocol

Every tool's installed hook script is a one-liner:

```bash
#!/bin/bash
# ~/.mur/hooks/on-prompt.sh
exec mur hook prompt --tool "${MUR_TOOL:-claude}" < /dev/stdin
```

`mur hook` parses tool-specific input schemas:

| Tool         | event names                                                        | input shape                              | async strategy                                |
|--------------|---------------------------------------------------------------------|------------------------------------------|----------------------------------------------|
| Claude Code  | UserPromptSubmit / PreToolUse / PostToolUse / Stop / SessionStart   | `{prompt, tool_name, tool_input, ...}`   | native `async: true` + `asyncRewake: true`   |
| Auggie       | (same as Claude Code)                                               | same                                     | native, once Anthropic spec is mirrored      |
| Amp          | (same as Claude Code)                                               | same                                     | native                                        |
| Gemini CLI   | BeforeAgent / AfterTool / SessionEnd                                | `{prompt, tool, ...}`                    | fallback: `fork → exit 0`                    |
| Cursor       | beforeSubmitPrompt / beforeShellExecution / stop                    | `{prompt, command, ...}`                 | fallback                                      |
| Copilot CLI  | userPromptSubmitted / preToolUse / postToolUse / sessionEnd         | `{prompt, tool, ...}`, `timeoutSec`      | fallback, `timeoutSec: 5`                    |
| OpenCode     | session.created / tool.execute.after / session.updated              | TS plugin events                         | natural Promise async                        |
| Codex CLI    | (no hooks; only AGENTS.md)                                          | n/a                                      | file-based: write to `~/.mur/context.md`     |
| File-based   | (Aider/Cline/Windsurf/Zed/Junie/Trae)                               | n/a                                      | file-based                                    |

**Async fallback contract** (for tools without `async: true`):

```rust
// mur hook
fn handle_async_fallback() {
    let event = read_stdin_normalised()?;
    if !is_critical_path(&event) {
        unsafe { libc::fork() };
        if is_child() {
            // parent already returned exit 0
            run_heavy_work_then_exit();
        }
    }
    write_stdout_additional_context();
    std::process::exit(0);  // < 50 ms
}
```

`mur hook` parent process always returns < 50 ms. The child performs queue write and any synchronous quick-path work before exiting independently.

**Daemon lifecycle** (`murmurd`):

- **Install:** `mur init --hooks` writes `~/Library/LaunchAgents/run.mur.murmurd.plist` (macOS) or `~/.config/systemd/user/murmurd.service` (Linux). `KeepAlive: true`. Auto-start on login.
- **Health:** lockfile `~/.mur/murmurd.lock` with PID + heartbeat timestamp. `mur hook` checks heartbeat freshness (< 30 s); stale lock triggers `mur murmurd --detach` respawn.
- **Degraded mode:** if respawn fails, `mur hook` falls back to L0-only synchronous quick path. Learn / emerge / evolve are deferred until daemon comes back. `mur hook stats` reports degraded state.
- **Idle behavior:** Tokio runtime parks worker threads when queue is empty; idle RSS target < 30 MB.

## 7. Milestones

Pre-launch — no backward-compat layer. Each milestone is a self-contained PR.

```
M0 — Adaptive Gate                    [~500 LOC]
  • mur-core/src/inject/gate.rs (intent + tool_signal + noise_filter wrap)
  • inject_cmd routes through gate; score < 0.3 returns empty
  • Unit tests + 100-query golden set, accuracy ≥ 0.85
  • Effect: greetings/ack stop triggering injection immediately

M1 — `mur hook` unified entry         [~1200 LOC]
  • mur-core/src/cmd/hook.rs (subcommand: hook prompt|tool|stop|session-start)
  • per-tool stdin parsers (Claude Code / Gemini / Cursor / Copilot / OpenCode)
  • mur init --hooks rewrites all 9 tool configs to `exec mur hook ...`
  • Old shell scripts (on-prompt.sh / on-tool.sh / on-stop.sh) deleted
  • Cross-tool integration tests via fixture stdin payloads

M2 — Capability Index (L0)            [~400 LOC]
  • mur-core/src/inject/index.rs builds ~/.mur/index/capabilities.json
  • SessionStart hook injects compacted index (project-filtered)
  • Replaces previous mur context --compact behavior
  • Token budget enforcement: hard cap 600 tokens

M3 — murmurd daemon                   [~1500 LOC, new crate]
  • mur-daemon/ crate (Tokio + notify watcher)
  • subscribes to ~/.mur/queue/events.jsonl
  • runs retrieve / extract / emerge / evolve in worker pool
  • writes ~/.mur/inbox/<session>.md
  • mur init --hooks installs launchd / systemd unit
  • Stale-lock respawn + degraded-mode fallback in mur hook

M4 — Tool-call-aware L2               [~500 LOC]
  • PreToolUse(Edit|Write|Bash) reads inbox and emits additionalContext
  • Claude Code: async: true on heavy hooks, asyncRewake on Stop
  • Workflow trigger detection (forced L1)
  • End-to-end test: simple prompt → 0 tokens; coding turn → L2 inject

M5 — Telemetry + tuning               [~300 LOC]
  • mur hook stats (skip rate / tier dist / latency p50/p95/p99 / inbox-hit rate)
  • One-week shadow run with logged decisions
  • Re-calibrate gate weights if SLO drift detected
```

Total: ~4400 LOC across 6 PRs. Estimated 3-4 weeks at one engineer.

## 8. Success criteria

| Dimension                  | Method                                                    | Target           |
|----------------------------|-----------------------------------------------------------|------------------|
| Recall (don't miss)        | 100 real "should-inject" queries from past sessions       | recall ≥ 0.95    |
| Precision (don't spam)     | 50 "should-skip" turns (greetings/ack/meta cmd)           | skip rate = 1.0  |
| Latency p99 (warm daemon)  | `mur hook prompt` end-to-end                              | < 100 ms         |
| Latency p99 (cold start)   | first hook after boot, daemon respawn                     | < 200 ms         |
| Learn coverage             | patterns extracted in daemon mode vs prior on-stop batch  | ratio ≥ 0.9      |
| L2 trigger rate            | L2 injects ÷ PreToolUse(Edit\|Write\|Bash) events         | 30 - 60 %        |
| Token efficiency           | average tokens injected per turn (across mixed sessions)  | ≤ 600            |

## 9. Risks and mitigations

- **Daemon dies silently** → lockfile heartbeat + auto-respawn from `mur hook`; `mur hook stats` surfaces degraded state.
- **Gate too aggressive** (skips real coding queries) → M5 telemetry on `skip_rate_on_dev_task`; SLO < 5 %; re-tune weights.
- **Inbox staleness** under fast-paced edits → 5 min TTL + cosine-similarity check on prefetch_hit; quick-path L1-only fallback if stale.
- **Tool format drift** (Anthropic / Google / GitHub change schemas) → `mur hook` parser is per-tool with a small surface; CI runs against fixture payloads from each upstream.
- **macOS launchd vs Linux systemd vs WSL2 vs containers** → fallback path in `mur hook` self-respawns daemon as detached child process if no init system is detected.
