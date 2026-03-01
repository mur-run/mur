# MUR

[![Go Version (v1)](https://img.shields.io/github/go-mod/go-version/mur-run/mur-core?label=v1%20%28Go%29)](https://github.com/mur-run/mur-core)
[![Release](https://img.shields.io/github/v/release/mur-run/mur)](https://github.com/mur-run/mur/releases)

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
# Install
brew tap mur-run/tap && brew install mur

# Create your first pattern
mur new "prefer-swift-testing"

# Search your patterns
mur search "testing conventions"

# Sync to all your AI tools
mur sync
```

<details>
<summary>Other install methods</summary>

```bash
# From source
cargo install --git https://github.com/mur-run/mur.git

# Upgrading from v1 (Go)?
mur migrate
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
 "Using @Test as you prefer, with #expect()..."           │
              │                                            │
              ▼                                            │
 Post-session feedback loop ◄─────────────────────────────┘
 • Contradiction detection (English + Chinese)
 • Pattern reinforcement / decay
 • Cross-session emergence clustering
```

Patterns start as **Draft**, get promoted through **Emerging → Stable → Canonical** based on real usage, and automatically decay if unused. No junk accumulates.

## Features

| Feature | Description |
|---------|-------------|
| **Continuous Learning** | Extract patterns from Claude Code, Gemini CLI, Cursor, and other AI sessions |
| **Universal Sync** | 16+ tools: Claude Code, Gemini CLI, Auggie, Cursor, Copilot CLI, OpenClaw, OpenCode, Amp, Codex, Aider, Windsurf, Zed, Junie, Trae, Cline, Amazon Q |
| **Semantic Search** | LanceDB vector search + BM25 hybrid ranking (Ollama or OpenAI embeddings) |
| **Pattern Maturity** | Draft → Emerging → Stable → Canonical with auto-promotion and demotion |
| **Automatic Decay** | Exponential half-life system — unused patterns fade, pinned patterns persist |
| **Feedback Loop** | Post-session contradiction and reinforcement detection (English + Chinese) |
| **Diagram Attachments** | Mermaid and PlantUML diagrams inline-injected with patterns |
| **Cross-Session Emergence** | Behavior fingerprinting + Jaccard clustering detects recurring themes |
| **Knowledge Intelligence** | Co-occurrence tracking, composition suggestions, pattern decomposition |
| **Workflow Engine** | Separate from patterns — multi-step workflows with variables and permissions |
| **Pattern Linking** | Zettelkasten-style bidirectional links between patterns |
| **Pattern Tiers** | Session (14d half-life) → Project (90d) → Core (365d) |
| **Security** | Injection scanning, trust levels, content hashing |
| **Local First** | All data on your machine. YAML is source of truth, vector index is rebuildable |

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

v2 patterns have dual-layer content (technical + principle), evidence tracking, confidence scoring, and bidirectional links — a significant upgrade from v1's flat format.

## CLI Commands

| Command | Description |
|---------|-------------|
| `mur new <name>` | Create a new pattern |
| `mur search <query>` | Semantic + keyword hybrid search |
| `mur inject` | Inject matching patterns into context |
| `mur context` | Show what would be injected |
| `mur sync` | Sync patterns to all configured AI tools |
| `mur stats` | Pattern library statistics |
| `mur dashboard` | Overview of patterns, maturity, and health |
| `mur evolve` | Run maturity promotion/demotion cycle |
| `mur emerge` | Detect cross-session emergence clusters |
| `mur suggest` | Get composition and decomposition suggestions |
| `mur feedback auto` | Run post-session contradiction detection |
| `mur feedback helpful/unhelpful` | Manual feedback on last injection |
| `mur learn extract` | Extract patterns from AI session transcripts |

<details>
<summary>All commands</summary>

```
mur
├── new                Create pattern
├── search             Semantic + BM25 hybrid search
├── inject             Inject patterns into context
├── context            Preview injection context
├── sync               Sync to AI tools
├── stats              Library statistics
├── dashboard          Pattern health overview
├── evolve             Run maturity lifecycle
├── emerge             Cross-session emergence detection
├── suggest            Composition / decomposition suggestions
├── feedback
│   ├── auto           Post-session contradiction detection
│   ├── helpful        Mark last injection as helpful
│   └── unhelpful      Mark last injection as unhelpful
├── workflow
│   ├── list           List workflows
│   ├── show           Show workflow details
│   └── new            Create workflow
├── session
│   ├── start          Start recording
│   ├── stop           Stop recording
│   ├── record         Record an event
│   ├── status         Current session status
│   └── list           List past sessions
├── pattern show       Show pattern details
├── learn extract      Extract patterns from transcripts
├── migrate            Migrate from v1
├── gc                 Garbage collect expired patterns
├── pin                Pin pattern (skip decay)
├── mute               Mute pattern from injection
├── boost              Boost pattern importance
├── promote            Manually promote maturity
├── deprecate          Deprecate a pattern
├── reindex            Rebuild vector index
├── links              Show pattern links
└── community          Browse community patterns
```

</details>

## Semantic Search

Find patterns by meaning, not just keywords:

```bash
# With OpenAI (recommended, ~$0.001 per 200 patterns)
export OPENAI_API_KEY=sk-...
mur reindex

# With Ollama (free, local)
ollama pull qwen3-embedding
mur reindex

# Search naturally
mur search "how to handle authentication errors"
# → error-handling-auth (0.84)
# → retry-with-backoff  (0.71)
```

MUR uses LanceDB for vector storage and combines semantic similarity with BM25 keyword scoring for hybrid ranked results.

## v1 → v2

| | v1 (Go) | v2 (Rust) |
|---|---|---|
| Retrieval | Tag matching, full dump | Semantic + BM25 hybrid, scored top-k |
| Lifecycle | Patterns live forever | Auto promote/demote with exponential decay |
| Feedback | None | Full closed loop — inject → track → evolve |
| Quality | No filter, junk accumulates | Noise filter + dedup + contradiction detection |
| Pattern format | Flat YAML | Dual-layer content + evidence + links |
| Storage | YAML only | YAML (truth) + LanceDB vector index |
| Emergence | None | Cross-session behavior fingerprinting |
| Intelligence | None | Co-occurrence tracking + composition suggestions |
| Binary | ~15MB | ~3.6MB (arm64 release) |
| Tests | ~40 | 200+ |

Upgrading? Just run `mur migrate`.

For the Go v1, see [mur-core](https://github.com/mur-run/mur-core).

## Architecture

```
mur-common/     Shared types (Pattern, Workflow, KnowledgeBase, Config, Event)
mur-core/       CLI + all logic (capture → store → retrieve → evolve → inject)
```

~12,000 lines of Rust. 200+ tests. YAML as source of truth with a rebuildable LanceDB vector index.

## Configuration

```yaml
# ~/.mur/config.yaml
search:
  provider: openai              # openai | ollama
  model: text-embedding-3-small
  api_key_env: OPENAI_API_KEY

tools:
  claude:
    enabled: true
  gemini:
    enabled: true
```

## Privacy & Security

- **100% local** — all patterns stored on your machine (`~/.mur/`)
- **No telemetry** — no usage data collected
- **Injection scanning** — patterns are checked for prompt injection attempts
- **Content hashing** — tamper detection on pattern files
- **Trust levels** — community patterns are sandboxed from local ones

## System Requirements

- **Platforms:** macOS, Linux, Windows
- **Optional:** Ollama (for local embeddings and LLM extraction)
- **Optional:** OpenAI API key (for cloud embeddings)

## Contributing

Issues and PRs welcome.

```bash
git clone https://github.com/mur-run/mur.git
cd mur
cargo test --workspace
```

## License

MIT
