# MUR

[![Release](https://img.shields.io/github/v/release/mur-run/mur)](https://github.com/mur-run/mur/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

**Continuous learning for AI assistants.**

MUR captures patterns from your coding sessions and injects them into your AI tools. Your assistant learns your conventions, remembers your fixes, and gets smarter over time — automatically.

## Why MUR?

Every time you use an AI CLI, you start from scratch. It forgets your project conventions, coding patterns, and past discoveries.

MUR remembers.

```
Without MUR:
  You: "Use Swift Testing instead of XCTest"
  ... 3 days later ...
  You: "Use Swift Testing instead of XCTest" (again)

With MUR:
  AI already knows your testing preferences.
  Zero repetition. Continuous learning.
```

## Quick Start

```bash
# Install via Homebrew (macOS)
brew tap mur-run/tap && brew install mur

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
# From source (requires Rust toolchain)
cargo install --git https://github.com/mur-run/mur.git

# Or clone and build
git clone https://github.com/mur-run/mur.git
cd mur
cargo build --release
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
| **Continuous Learning** | Extract patterns from Claude Code, Gemini CLI, Cursor, and other AI sessions |
| **Universal Sync** | 16+ tools: Claude Code, Gemini CLI, Auggie, Cursor, Copilot CLI, OpenClaw, OpenCode, Amp, Codex, Aider, Windsurf, Zed, Junie, Trae, Cline, Amazon Q |
| **Semantic Search** | LanceDB vector search + BM25 hybrid ranking |
| **Embedded Dashboard** | Built-in web UI for pattern management, workflow editing, and session review |
| **Workflow Engine** | Multi-step workflows with variables, tools, and session extraction |
| **Session Recording** | Record AI sessions, review events, extract reusable workflows |
| **Pattern Maturity** | Draft → Emerging → Stable → Canonical with auto-promotion and demotion |
| **Automatic Decay** | Exponential half-life — unused patterns fade, pinned patterns persist |
| **Multi-language** | Dashboard UI in English, 繁體中文, 简体中文 |
| **Local First** | All data on your machine. YAML is source of truth |

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
```

Run `mur init` for an interactive setup wizard.

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

~12,000 lines of Rust. YAML as source of truth with a rebuildable LanceDB vector index.

## System Requirements

- **macOS** (Apple Silicon or Intel) — primary platform
- **Linux** — supported
- **Optional:** [Ollama](https://ollama.com) for local embeddings (recommended)
- **Optional:** OpenAI API key for cloud embeddings

## Contributing

Issues and PRs welcome.

```bash
git clone https://github.com/mur-run/mur.git
cd mur
cargo test --workspace
```

## License

MIT
