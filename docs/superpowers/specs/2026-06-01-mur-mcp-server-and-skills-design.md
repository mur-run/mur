# MUR MCP Server + AI Tool Skills — Design

**Date:** 2026-06-01
**Status:** Draft
**Related:** `docs/superpowers/specs/2026-06-01-cost-router-orchestrator-design.md` (MCP tool consumption, not provisioning)

## 1. Overview

MUR currently integrates with AI tools via file-based hooks (`mur sync` writes tool-specific configs). This design adds two new integration layers:

1. **MCP Server** — A thin MCP server (6 tools, ~4,800 schema tokens) exposing interactive lookup commands so AI tools can call MUR mid-conversation.
2. **Skills** — MUR skill manifests that teach AI agents *when and why* to run MUR CLI commands, consumed at SessionStart and via hook triggers.

The guiding principle: **fire-and-forget commands → hooks; interactive lookups → MCP tools; teaching/guidance → skills.**

## 2. Research Foundation

Industry consensus (2025–2026) on MCP tool design:

| Finding | Source |
|---|---|
| Optimal 5–15 tools per server; 50 max across all servers | Prefect/fastmcp, Harness v2 |
| 130+ tools → 11 generic verbs + registry dispatch = 94% context reduction | Harness Engineering (Mar 2026) |
| Tool Search progressive disclosure: 3 bridge tools, ~300 tokens, +8–25% accuracy | Anthropic, Amazon Prime Video |
| Schema optimization: short descriptions, simple params, no nested objects | Pydantic MCP guide (2026) |
| Tools designed around agent workflow phases, not API endpoints | North (outfitter-dev) |
| Context decoupled code execution (CE-MCP) — single `run_python` tool | Red Hat, Anthropic, Cloudflare |

**For MUR's scale,** 6 tools is well within the ideal 5–15 range. No progressive discovery layer needed initially — add it when/if tools exceed 15.

## 3. Architecture

```
┌─────────────────────────────────────────────────────┐
│              Skills (Instruction Layer)              │
│  skill.yaml manifests. Category: context/command.    │
│  Consumed at SessionStart + hook events.             │
│  Teaches AI WHEN and WHY to use MUR commands.        │
└────────────────────┬────────────────────────────────┘
                     │
┌────────────────────▼────────────────────────────────┐
│           MCP Server (Execution Layer)               │
│  stdio JSON-RPC. 6 tools. ~4,800 schema tokens.      │
│  Exposes interactive lookup commands.                │
│  Binary: `mur-mcp-server` (new crate or binary).     │
└────────────────────┬────────────────────────────────┘
                     │
┌────────────────────▼────────────────────────────────┐
│          Claude Code Hooks (Trigger Layer)           │
│  PostToolUse(Bash git commit) → mur project index    │
│  SessionStart → mur hook context                     │
│  Stop → mur session out --action analyze             │
└─────────────────────────────────────────────────────┘
```

### 3.1 Why Not One Big MCP Server?

Exposing all 30+ MUR commands as MCP tools would consume ~24,000+ tokens in tool schemas alone — 12% of a 200K context window — before any work begins. This violates the core MCP design principle: **tool count O(1), capabilities O(n).**

Instead, the skills layer carries the "what commands exist" knowledge at near-zero token cost (a few hundred tokens of system prompt injection), and only the 6 most interactive commands become MCP tools.

### 3.2 Binary Decision: New Crate vs. Built Into `mur`

**Decision: New `mur-mcp-server` binary in the workspace.**

Rationale:
- MCP server is a long-running stdio process — distinct lifecycle from one-shot `mur` CLI
- Keeps `mur-core` free of MCP protocol dependencies (JSON-RPC, stdio framing)
- Can be started/stopped independently by AI tools
- Same pattern as `mur-agent-runtime` (separate binary, BusyBox symlink optional)

Alternative considered: adding `mur serve --mcp` to the existing `serve` command. Rejected because `serve` binds a TCP port for the web dashboard — MCP over stdio is an entirely different transport.

## 4. MCP Tools

### 4.1 Tool Definitions

All tools are **read-only**. No mutation via MCP.

#### `mur_notes_search`

```
Description: Search MUR notes and patterns by keyword query.
             Returns ranked results with name, description, maturity, and relevance score.
Parameters:
  - query (string, required): Search query
  - limit (integer, optional, default=5): Max results (1-10)
```

Wraps: `mur notes search <query> --limit <limit>`

#### `mur_notes_show`

```
Description: Load a specific note or pattern by name. Returns full body, metadata, maturity, and tags.
Parameters:
  - name (string, required): Note name (exact match)
```

Wraps: `mur notes show <name>`

#### `mur_project_search`

```
Description: Search indexed project source code using hybrid vector+BM25. Returns code snippets with file paths and line numbers.
             Only works after `mur project index` has been run for the project.
Parameters:
  - query (string, required): Search query
  - project (string, optional): Project name filter (defaults to current project)
  - limit (integer, optional, default=5): Max results (1-10)
```

Wraps: `mur project search <query> --limit <limit> [--project <name>]`

#### `mur_project_status`

```
Description: Show which projects are indexed and their indexing status (chunk count, last indexed, freshness). Use before project search to check if a project is indexed.
Parameters: none
```

Wraps: `mur project status` + `mur project list` (merged output)

#### `mur_agent_status`

```
Description: List configured MUR agents with their running state, health, and transport. Use to check if agents are online before sending A2A messages.
Parameters:
  - name (string, optional): Filter by agent name (shows detail for one agent; lists all if omitted)
```

Wraps: `mur agent list` + `mur agent status <name>` (merged)

#### `mur_hook_context`

```
Description: Get patterns that MUR would inject for the current project context. Returns top-ranked patterns within token budget. Use at session start or when switching contexts.
Parameters:
  - query (string, optional): Override auto-detected context query
  - compact (boolean, optional, default=false): Return fewer patterns in shorter format
  - budget (integer, optional, default=2000): Token budget for returned content
```

Wraps: `mur hook context --json [--query <q>] [--compact] [--budget <n>]`

### 4.2 Tool Schema Design Principles

1. **Flat parameters only** — no nested objects. AI models handle flat params better.
2. **Short descriptions** — ≤3 sentences per tool. Keeps schema tokens low.
3. **Sensible defaults** — every optional parameter has a useful default.
4. **Error messages guide recovery** — errors include suggestions for what to try next (e.g., "Project not indexed. Run `mur project index` first.").

### 4.3 Token Budget

| Item | Tokens |
|---|---|
| 6 tool schemas @ ~800 tokens each | ~4,800 |
| As % of 200K context window | 2.4% |
| As % of 128K context window | 3.75% |

Well within the recommended <5% tool overhead target.

### 4.4 Transport

- **stdio** JSON-RPC 2.0 (standard MCP transport)
- No authentication (local-only; same security model as `mur` CLI)
- The AI tool spawns `mur-mcp-server` as a child process

### 4.5 Crate Structure

```
mur-mcp-server/
  Cargo.toml          # depends on mur-core (for cmd functions), mcp-sdk
  src/
    main.rs           # stdio listener, JSON-RPC framing
    tools.rs          # tool schema definitions + dispatch to mur-core::cmd
    server.rs         # MCP lifecycle (initialize, tools/list, tools/call)
```

## 5. Skills

### 5.1 New Skills

#### `mur-project-index`

```yaml
name: mur-project-index
version: 0.1.0
publisher: human:mur
description: "Index a project's source code for semantic search. Run after git commits."
category: context
hosts: [all]
content:
  abstract: |
    Run `mur project index` to rebuild the codebase index.
    This enables `mur project search` for semantic code search.
  context: |
    # mur-project-index — Keep Codebase Index Fresh

    After committing or pushing code, run:
    ```
    mur project index
    ```

    For large projects add `--background` to index asynchronously.
    Use `--rebuild` to force a full reindex (ignores mtime cache).

    Check index status with: `mur project status`
tags: [mur, project, indexing, builtin]
triggers:
  - type: keyword
    pattern: "(index|reindex|rebuild index)"
  - type: manual
priority: normal
```

#### `mur-project-remove`

```yaml
name: mur-project-remove
version: 0.1.0
publisher: human:mur
description: "Remove a stale project index to free disk space."
category: command
content:
  abstract: |
    Run `mur project remove [--path <path>]` to delete an indexed project.
  command: "mur project remove"
tags: [mur, project, cleanup, builtin]
triggers:
  - type: manual
priority: low
```

#### `mur-session-remove`

```yaml
name: mur-session-remove
version: 0.1.0
publisher: human:mur
description: "Remove stale session recordings to free disk space."
category: command
content:
  abstract: |
    Run `mur session remove <id>` to delete a session recording.
    List sessions first with `mur session list`.
  command: "mur session remove"
tags: [mur, session, cleanup, builtin]
triggers:
  - type: manual
priority: low
```

#### `mur-agent-manage`

```yaml
name: mur-agent-manage
version: 0.1.0
publisher: human:mur
description: "Manage MUR agent lifecycle: create, start, stop, export."
category: workflow
content:
  abstract: |
    Guide through MUR agent lifecycle management.
  procedure:
    steps:
      - description: To list agents, run `mur agent list` or use the MCP tool `mur_agent_status`
      - description: To create a new agent, run `mur agent create <name>` and follow the wizard
      - description: To start an agent, run `mur agent start <name>`
      - description: To stop an agent, run `mur agent stop <name>`
      - description: To check agent health, use `mur_agent_status(name="<name>")`
      - description: To export an agent as .muragent, run `mur agent export <name>`
tags: [mur, agent, builtin]
triggers:
  - type: command
    pattern: "/mur-agent"
  - type: keyword
    pattern: "(mur agent|manage agent|create agent|export agent)"
priority: normal
```

### 5.2 Existing Skills to Update

| Skill | Changes |
|---|---|
| `mur-context` | Mention that `mur_notes_search` and `mur_notes_show` MCP tools are available for finer-grained lookup. Add project indexing context. |
| `mur-in` | Add `mur session in` as primary command (keep `/mur-in` trigger for back compat). |
| `mur-out` | Add Stop hook trigger for auto-analysis. Add `mur session out --action analyze`. |

### 5.3 Trigger Design

Skills use four trigger types (existing `TriggerKind` enum):

| Trigger | Use Case | Example |
|---|---|---|
| `SessionStart` | Inject context at session start | `mur-context` |
| `Command` | Slash command triggers | `/mur-in`, `/mur-agent` |
| `Keyword` | Natural-language triggers | "index the project", "manage agent" |
| `Manual` | AI decides when to use | cleanup commands, rare operations |

## 6. Hooks

### 6.1 Hook Configuration

Written to `~/.mur/hooks.json` and applied by `mur init --hooks`:

```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "startup|resume",
        "hooks": [
          {
            "type": "command",
            "command": "mur hook context --quiet",
            "timeout": 10000
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "mur hook tool --tool claude",
            "timeout": 5000
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "mur hook stop --tool claude",
            "timeout": 30000
          }
        ]
      }
    ]
  }
}
```

The `mur hook tool` handler detects git commits and triggers `mur project index --quiet --background` when appropriate. The `mur hook stop` handler triggers session analysis.

### 6.2 Hook Logic (in `cmd/hook.rs`)

- **`hook tool`**: Receives tool call data via stdin. If the tool is Bash and the command contains `git commit` or `git push`, spawn `mur project index --quiet --background`. Otherwise no-op.
- **`hook session-start`**: Calls `mur hook context` and outputs any patterns for injection.
- **`hook stop`**: Triggers `mur session out --action analyze` if a session is active.

## 7. Implementation Phases

### Phase 1 — MCP Server Scaffolding (this PR)

1. Create `mur-mcp-server/` crate in workspace
2. Implement stdio JSON-RPC transport
3. Implement MCP lifecycle: `initialize`, `tools/list`, `tools/call`
4. Wire `mur_notes_search`, `mur_notes_show` (simplest — pure read, no side effects)
5. Integration test: spawn server, list tools, call `mur_notes_search`

### Phase 2 — Remaining MCP Tools

6. Wire `mur_project_search`, `mur_project_status`
7. Wire `mur_agent_status`
8. Wire `mur_hook_context`
9. Integration tests for all tools

### Phase 3 — Skills

10. Create 4 new skill manifests in `~/.mur/skills/`
11. Update 3 existing skills (`mur-context`, `mur-in`, `mur-out`)
12. Test skill injection at SessionStart

### Phase 4 — Hooks

13. Update `cmd/hook.rs` with git-commit detection logic
14. Generate hook configs via `mur init --hooks`
15. End-to-end test: session start → context injection → git commit → auto-index → stop → auto-analyze

## 8. Testing Strategy

| Layer | Tests |
|---|---|
| MCP protocol | Unit tests for JSON-RPC framing, `initialize` handshake, error responses |
| Tool dispatch | Each tool: valid params → correct CLI invocation; invalid params → structured error with suggestion |
| Integration | Spawn `mur-mcp-server`, call `tools/list` → expect 6 tools; call each tool → expect valid response |
| Skills | Parse all new skill manifests; validate against schema; test trigger matching |
| Hooks | Unit test git-commit detection regex; integration test full hook pipeline |
| Token budget | Snapshot test: `tools/list` response must stay under 5,000 tokens |

## 9. Future Considerations

- **Tool Search / progressive disclosure**: If MUR tools grow beyond 15, add the 3-tool bridge pattern (`tool_search`, `tool_describe`, `tool_call`) rather than exposing more tools directly.
- **Composite tools**: If common patterns emerge (e.g., "search notes + search codebase + show context"), consider a single `mur_research` composite tool that chains multiple commands server-side.
- **MCP resources**: Expose patterns and codebase chunks as MCP resources (URI-addressable) for tools that prefer resource-based access.
- **Auth for remote MUR**: If MUR ever supports remote MCP (SSE transport), add token-based auth. Not needed for local stdio.
