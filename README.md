# MUR

[![Release](https://img.shields.io/github/v/release/mur-run/mur)](https://github.com/mur-run/mur/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

**The learning layer for AI coding tools.**

MUR is the only system that captures patterns from your coding sessions, evolves them over time, and injects them into 16+ AI tools automatically. Unlike static rules files or generic memory APIs, MUR's patterns mature, decay, and adapt — so your AI assistants continuously get smarter.

## Why MUR?

Every AI coding tool starts from scratch. You repeat the same instructions, lose hard-won discoveries, and maintain separate config files for each tool.

**MUR closes the loop.**

```
Session 1:  You correct the AI → MUR captures the pattern (Draft)
Session 5:  Pattern auto-promoted to Emerging (validated by usage)
Session 20: Pattern reaches Stable — injected with high confidence
Month 3:    Unused patterns decay. Active ones get promoted to Canonical.
```

### How MUR compares

| | **Rules sync tools** (ai-rulez, rulesync) | **Agent memory** (Mem0, Zep, Letta) | **MUR** |
|---|---|---|---|
| Multi-tool sync | Yes (static files) | No | Yes (16+ tools) |
| Learns from sessions | No | Generic memory | Developer-specific patterns |
| Pattern evolution | No | No | Decay, maturity lifecycle, GEP |
| Feedback loop | No | No | Reinforcement + contradiction detection |
| Data format | Markdown/YAML | Opaque DB / API | YAML + Git-friendly |
| Privacy | Local | Cloud-dependent | 100% local-first |

> **In short:** Rules sync tools write config files but don't learn. Memory frameworks store data but don't evolve it. MUR does both — and connects them in a closed loop.

### Agents (P0a — `murmur`)

`mur agent ...` spawns and manages long-running per-agent processes that
speak A2A v0.3. Each agent has its own profile, prompt, MCP servers,
skills, entitlements, and Unix socket; the runtime is one BusyBox-style
binary symlinked once per agent (`mur_agent_<name>` → `mur-agent-runtime`).
See [`mur-agent-runtime/README.md`](mur-agent-runtime/README.md) for the
walkthrough (`create / start / send / card / list / export`).

## Quick Start

```bash
# macOS / Linux — one-liner
curl -fsSL https://mur.run/install.sh | sh

# Interactive setup — configures embeddings, hooks, and sync targets
mur init

# Create your first pattern
mur new "prefer-swift-testing"

# Sync to all your AI tools (Claude Code, Gemini CLI, Cursor, etc.)
mur sync
```

<details>
<summary>Other install methods</summary>

```bash
# Windows (PowerShell)
irm https://mur.run/install.ps1 | iex

# macOS — signed installer
# Download mur-aarch64-apple-darwin.dmg from the Releases page and double-click.

# Homebrew (macOS arm64)
brew install mur-run/tap/mur

# crates.io
cargo install mur

# Keep up to date
mur update           # check + install latest
mur update --check   # check only, don't install
```

</details>

## How It Works

```
 You use AI CLI normally
 $ claude "write tests for auth module"
              │
              ▼
 MUR hooks inject relevant patterns ──────────────────────┐
 [swift-testing, error-handling, auth-conventions]         │
              │                                            │
              ▼                                            │
 AI responds with your preferences                        │
              │                                            │
              ▼                                            │
 Post-session feedback loop ◄─────────────────────────────┘
 • Contradiction detection
 • Pattern reinforcement / decay
 • Cross-session emergence clustering
```

Patterns start as **Draft**, get promoted through **Emerging → Stable → Canonical** based on real usage, and automatically decay if unused. No junk accumulates.

## Features

| Feature | Description |
|---------|-------------|
| **Closed-Loop Learning** | Session recording → pattern extraction → injection → feedback → evolution. No other tool does all five. |
| **Pattern Evolution Engine** | Time decay (exponential half-life), maturity lifecycle (Draft→Canonical), GEP-based genetic evolution, auto-promotion/demotion. **No competitor has this.** |
| **Universal Sync** | 16+ tools: Claude Code, Gemini CLI, Auggie, Cursor, Copilot CLI, OpenClaw, OpenCode, Amp, Codex, Aider, Windsurf, Zed, Junie, Trae, Cline, Amazon Q |
| **Hybrid Semantic Search** | Vector similarity (70%) + BM25 keyword (30%) + 6-factor scoring. More precise than pure vector search. |
| **Workflow Engine** | Multi-step workflows with variables, tools, and session extraction |
| **Embedded Dashboard** | Built-in web UI for pattern management, workflow editing, and session review |
| **YAML + Git Friendly** | Human-readable patterns, version-controllable, no opaque database lock-in |
| **100% Local First** | All data on your machine. Zero telemetry. Injection scanning + content hashing for security. |
| **Multi-language** | Dashboard UI in English, 繁體中文, 简体中文 |

## Dashboard

MUR includes a built-in web dashboard for visual management:

```bash
# Start the dashboard
mur serve

# Opens at http://localhost:3847
```

The dashboard provides:
- **Pattern management** — view, edit, filter by maturity/tier/tags, bulk operations
- **Workflow editor** — create, edit, and search workflows with semantic search
- **Session review** — browse recordings, extract workflows from sessions
- **Statistics** — confidence distribution, maturity breakdown, decay warnings
- **Settings** — data source config, export/import, theme toggle

## Workflows

Workflows are reusable multi-step procedures extracted from your sessions:

```bash
# Record a session
mur session start
# ... do your work in an AI CLI ...
mur session stop

# Extract a workflow from the session (via dashboard)
mur serve
# → Sessions → Select session → "Extract Workflow"

# Or create manually
mur workflow new "deploy-staging"

# Search workflows (semantic + keyword)
mur workflow search "deploy process"

# Show workflow as AI-readable prompt
mur workflow show "deploy-staging" --md

# Run a workflow (outputs executable prompt for AI)
mur run "deploy"
```

Workflows support:
- **Variables** — string, url, path, number, bool, array types with defaults
- **Tool detection** — auto-detects agent-browser, MCP servers, etc.
- **Smart extraction** — heuristic title, description, and variable detection from session recordings

## CLI Commands

```
mur
├── init               Interactive setup wizard
├── new <name>         Create a new pattern
├── search <query>     Semantic + keyword hybrid search
├── inject             Inject matching patterns into context
├── context            Preview what would be injected
├── sync               Sync patterns to all AI tools + auto-reindex
├── serve              Start the web dashboard
├── stats              Pattern library statistics
├── evolve             Run maturity promotion/demotion cycle
├── emerge             Detect cross-session emergence clusters
├── suggest            Composition / decomposition suggestions
├── feedback
│   ├── auto           Post-session contradiction detection
│   ├── helpful        Mark last injection as helpful
│   └── unhelpful      Mark last injection as unhelpful
├── workflow
│   ├── list           List all workflows
│   ├── show <name>    Show workflow details (--md for AI format)
│   ├── search <q>     Semantic search workflows
│   └── new <name>     Create a new workflow
├── skill
│   ├── install        Install a skill
│   ├── list           List installed skills
│   ├── show <name>    Show skill details
│   └── recombine <a> <b>  Combine two skills into new draft (M7b)
├── run <query>        Find and output workflow as executable prompt
├── session
│   ├── start          Start recording a session
│   ├── stop           Stop recording
│   ├── record         Record an event
│   ├── status         Current session status
│   └── list           List past sessions
├── learn extract      Extract patterns from AI transcripts
├── exchange
│   ├── import         Import pattern file (MKEF format)
│   └── export         Export pattern to MKEF
├── pattern show       Show pattern details
├── gc                 Garbage collect expired patterns
├── pin / mute / boost Pattern management shortcuts
├── promote            Manually promote maturity
├── deprecate          Deprecate a pattern
├── reindex            Rebuild vector search index
├── links              Show pattern links
└── community          Browse community patterns
```

## Semantic Search

Find patterns by meaning, not just keywords:

```bash
# With Ollama (free, local — recommended)
ollama pull qwen3-embedding:0.6b
mur reindex

# With OpenAI
export OPENAI_API_KEY=sk-...
mur reindex

# Search naturally
mur search "how to handle authentication errors"
# → error-handling-auth (0.84)
# → retry-with-backoff  (0.71)
```

> **Note:** Semantic search requires an embedding provider (Ollama or OpenAI). Without one, MUR falls back to keyword search. Run `mur init` to configure.

## Pattern Format

Patterns are YAML files in `~/.mur/patterns/`:

```yaml
schema: 2
name: swift-testing-macro
description: Prefer Swift Testing over XCTest
content:
  technical: |
    Use @Test macro instead of func test...()
    Use #expect() instead of XCTAssert
  principle: |
    Swift Testing is more expressive and supports async natively
tier: project
importance: 0.8
confidence: 0.7
maturity: stable
tags:
  languages: [swift]
  topics: [testing]
applies:
  languages: [swift]
```

## Configuration

```yaml
# ~/.mur/config.yaml
embedding:
  provider: ollama              # ollama | openai | gemini | anthropic
  model: qwen3-embedding:0.6b  # run mur init for options
  dimensions: 1024

llm:
  provider: anthropic
  model: claude-sonnet-4-20250514
  api_key_env: ANTHROPIC_API_KEY

ask:
  summarize_hits_enabled: true  # Enable abstractive compression in overflow cascade
  summarize_model: null         # Override model for Stage 1b (null → falls back to ask.model)
```

Run `mur init` for an interactive setup wizard.

### Ask Configuration

The `ask` section controls behavior of `mur ask` queries:

- **`summarize_hits_enabled`** (default `true`) — When context is tight, `mur ask` runs an overflow cascade that compresses the longest hits using an abstractive summarization stage. This trades accuracy for context efficiency. Results cache per-hit at `~/.mur/conversations/cache/abstractive/` to amortize LLM cost. Summarization has a hardcoded 5s per-hit timeout and soft-fails silently on error. Set to `false` to restore pre-3.5 behavior (drop history first).

- **`summarize_model`** (default `null`) — Override the LLM model used for Stage 1b summarization. When `null`, falls back to `ask.model`. Pair with a faster model like `qwen3:4b` to trade summarization accuracy for speed on tight-budget queries.

#### CLI overrides (per-invocation)

Override the `summarize_*` config keys for a single `mur ask` invocation without editing `~/.mur/config.yaml`:

- **`--no-summarize`** — Disable Stage 1b for this invocation. Overrides `ask.summarize_hits_enabled`. Useful for demos, benchmarks, or scripted comparisons.

- **`--summarize-model <model>`** — Override the Stage 1b model for this invocation. Overrides `ask.summarize_model`. Pair with a faster model like `qwen3:4b` to trade quality for speed on the summarize hop. The cache key includes the model name, so changing this flag produces a fresh cache entry.

The two flags are mutually exclusive: passing both errors at argument-parse time.

## AI Tool Integration

MUR syncs patterns to your AI tools via their native skill/rules systems:

| Tool | Sync Target |
|------|-------------|
| Claude Code | `~/.claude/CLAUDE.md` |
| Gemini CLI | `~/.gemini/GEMINI.md` |
| Cursor | `.cursor/rules/` |
| OpenClaw | `~/.agents/skills/mur/` |
| Auggie | `~/.augment/skills/mur/` |
| Others | See `mur init --hooks` |

```bash
# Sync once
mur sync

# Auto-inject before each AI session (via shell hooks)
mur init --hooks
```

## Privacy & Security

- **100% local** — all patterns stored on your machine (`~/.mur/`)
- **No telemetry** — zero usage data collected
- **Injection scanning** — patterns checked for prompt injection attempts
- **Content hashing** — tamper detection on pattern files
- **Trust levels** — community patterns sandboxed from local ones

## Architecture

```
mur-common/     Shared types (Pattern, Workflow, KnowledgeBase, Config)
mur-core/       CLI + server + all logic
```

- `mur-common/src/coordination/` — Shared coordination protocol types (Plan/Step/Microstep schema, Verify Gateway, conformance suite). Used by mur-runtime and mur-commander.

~12,000 lines of Rust. YAML as source of truth with a rebuildable LanceDB vector index.

## System Requirements

- **macOS** (Apple Silicon or Intel) — primary platform
- **Linux** — supported
- **Optional:** [Ollama](https://ollama.com) for local embeddings (recommended)
- **Optional:** OpenAI API key for cloud embeddings

## Companion mode (Phase 1.1)

Companion mode adds a relationship-keyed warm voice into an agent's `sys_prompt` and an opt-in proactive outbox. The voice is composed once from onboarding answers and can be ejected to disk for manual editing; the outbox sends occasional context-aware check-ins based on time-of-day situation weights and a bandit-state picker — all fully local, no cloud required.

```bash
mur agent create my-buddy --provider stub
mur agent companion init my-buddy --answers <fixture>
mur agent companion proactive enable my-buddy   # opt in to occasional check-ins
mur agent companion inbox my-buddy --unread-only
mur agent companion ack <msg-id> --good
```

| Subcommand | Description |
|---|---|
| `companion proactive enable\|disable` | Toggle proactive sends on/off |
| `companion quiet --for\|--until\|--off` | Pause proactive sends for a duration or until a timestamp |
| `companion voice eject\|rebuild\|diff` | Manage the composed `voice.md` file on disk |
| `companion templates eject [--scope]` | Eject embedded voice templates for editing |
| `companion content add <situation>` | Add entries to the content pool for a situation |
| `companion inbox [--unread-only]` | List messages; filter to unread |
| `companion ack <id> --good\|--bad\|--dismiss` | Rate or dismiss a message |
| `companion preview --situation <s> [--no-llm]` | Preview a generated message without sending |
| `companion why-did-you-message [<id>]` | Replay the event chain that triggered a message |
| `companion rhythm wipe` | Reset companion state (preserves voice config) |

## Contributing

Issues and PRs welcome.

```bash
git clone https://github.com/mur-run/mur.git
cd mur
cargo test --workspace
```

## License

MIT
