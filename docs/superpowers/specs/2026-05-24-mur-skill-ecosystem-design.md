# MuR Unified Skill Ecosystem — Design Spec

**Date**: 2026-05-24
**Status**: Draft
**Scope**: Unified skill authoring, storage, runtime injection, composition, registry, peer transfer, and agent-generated skills

## 1. Motivation

MuR currently has two separate "skills" concepts that share no code, format, or runtime behavior:

| | Built-in AI Tool Skills | Agent Skills |
|---|---|---|
| **Location** | `~/.mur/skills/<name>/SKILL.md` | `<agent_home>/skills/<name>.md` |
| **Purpose** | Teach AI coding assistants how to use mur | Extend mur agent capabilities |
| **Format** | Markdown with YAML frontmatter | Plain markdown |
| **Runtime injection** | Via hook system at session start | **Not injected** (advertised in Agent Card only) |
| **Composition** | None | None |
| **Discovery** | `mur sync` symlinks | Agent Card only |

This design unifies both into a single structured skill ecosystem supporting agent-to-agent skill transfer (peer learning + registry marketplace), human+agent co-authorship, and composable skills.

## 2. Skill Data Model

A skill is a self-contained unit of transferable knowledge with three progressive-disclosure layers.

### 2.1 Three-Layer Progressive Disclosure

```
+---------------------------------------------+
|  Layer 1: Manifest (always visible)          |  ~50 tokens
|  name, version, publisher, triggers, tags    |
+---------------------------------------------+
|  Layer 2: Abstract (injected at boot)        |  ~200 tokens
|  what it does, when to use, dependencies     |
+---------------------------------------------+
|  Layer 3: Body (loaded on trigger match)     |  ~2000 tokens
|  full procedure, variables, examples, errors |
+---------------------------------------------+
```

- **Layer 1** — Always visible. Broadcast in A2A Agent Cards. Registry search index.
- **Layer 2** — Injected into system prompt at session start (subject to token budget). Describes purpose, triggers, required dependencies, expected outcomes.
- **Layer 3** — Loaded on demand when the skill is triggered. Complete procedure steps, variable definitions, examples, error handling.

### 2.2 Canonical Structure

```yaml
# Canonical YAML representation
name: research-prices
version: 1.0.0
publisher: human:david
description: Search and compare product prices across e-commerce sites
category: workflow            # context | workflow | command | meta

content:
  abstract: |
    Searches product prices on e-commerce sites.
    Triggered by `/research-prices` or keywords like "查價格", "find prices".
    Requires web-browsing skill. Outputs a sorted price table.
  procedure:
    variables:
      - name: product_name
        type: string
        required: true
        description: Product to search for
      - name: target_sites
        type: array
        required: false
        default: ["pchome", "momo", "shopee"]
        description: Sites to search
    steps:
      - description: Navigate to first target site
        tool: browser.navigate
      - description: Search for product
        tool: browser.fill
      - description: Extract price from results
        tool: browser.extract
      - description: Repeat for remaining sites
      - description: Sort results by price and present table

requires:
  - name: web-browsing
    version: ">=1.0.0"

tags: [e-commerce, price, shopping, scraping]

triggers:
  - type: command
    pattern: "/research-prices"
  - type: keyword
    pattern: "(查價格|比價|find prices|price check)"
  - type: manual

priority: normal               # low | normal | high | critical
```

### 2.3 Content Modes

Publisher identifiers use the format `human:<name>` for human-authored skills or `agent:<agent_id>` for agent-generated skills. Agent IDs match the agent's directory name in `~/.mur/agents/`.

A skill has exactly one content mode:

| Mode | Field | Description | Example |
|------|-------|-------------|---------|
| `context` | `content.context` | Declarative knowledge for system prompt | mur-context |
| `workflow` | `content.procedure` | Step-by-step with variables and tool refs | mur-run |
| `command` | `content.command` | Single triggered action | mur-in, mur-out |

`meta` category skills describe other skills (for skill discovery and composition guidance) and use `context` mode.

### 2.4 Dual Surface Format

- **Canonical YAML** — Full structured representation. What agents generate and what the runtime consumes.
- **Markdown frontmatter** — Simplified authoring format for humans. Auto-converted to canonical YAML on save.

```markdown
---
name: research-prices
version: 1.0.0
publisher: human:david
description: Search and compare product prices
category: workflow
requires:
  - web-browsing>=1.0.0
tags: [e-commerce, price]
triggers:
  - command: /research-prices
  - keyword: (查價格|比價|find prices)
---

# research-prices

Searches product prices on e-commerce sites.

## Variables
- `product_name` (string, required) — Product to search for
- `target_sites` (array, default: pchome,momo,shopee)

## Steps
1. Navigate to target site
2. Search for product_name
3. Extract price
4. Repeat for remaining sites
5. Sort and present table
```

`mur skill validate` checks both formats. `mur skill fmt` converts between them.

## 3. Storage

### 3.1 On-Disk Layout

```
~/.mur/
  skills/                          # Global skills (AI tool injection)
    research-prices/
      skill.yaml                   # Canonical source of truth
      skill.lock                   # Resolved dependency versions
    mur-context/
      skill.yaml
    ...

  agents/
    <agent>/
      profile.yaml                 # skills: ["skills/research-prices.yaml", ...]
      skills/                      # Per-agent skills (agent runtime)
        research-prices.yaml       # Same format as global skills
        ...
```

Global skills are available to all AI tool sessions. Per-agent skills are scoped to a single agent. Both use the same format.

### 3.2 Lock File

```yaml
# skill.lock — generated on install, checked into version control
locked:
  web-browsing: 1.2.0
  data-table-export: 0.6.1
installed_at: 2026-05-24T10:30:00Z
```

Ensures reproducible dependency resolution. `mur skill update` bumps locked versions.

## 4. Runtime Injection

### 4.1 Agent Boot Sequence

```
Agent boot
  -> Read profile.skills (per-agent) + ~/.mur/skills/ (global)
  -> Parse each skill.yaml; extract Layers 1+2
  -> Classify by trigger type:
      session_start -> inject Layer 2 into system prompt
      command/keyword -> register in trigger index
      manual -> list in Agent Card only
  -> Apply token budget
  -> Assemble system prompt
```

### 4.2 Token Budget

Reuses mur's existing retrieval config in `~/.mur/config.yaml`:

```yaml
skills:
  max_skills_in_prompt: 5
  max_tokens: 2000
  priority_order: [global, agent]   # or [agent, global]
```

Layer 3 (body) is excluded from this budget — it loads on trigger match, replacing the skill's Layer 2 abstract in context.

### 4.3 Trigger Matching

On each user prompt:
- Scan registered command triggers → match `/command` prefix
- Scan registered keyword triggers → match regex against prompt text
- On match → load skill's Layer 3 (body) into context
- No match → no action

### 4.4 Code Changes

| File | Change |
|------|--------|
| `mur-common/src/skill.rs` | New `Skill` struct + parser |
| `mur-agent-runtime/src/profile.rs` | `Profile::load()` reads skill yaml |
| `mur-agent-runtime/src/supervisor.rs` | `with_system_prompt()` injects Layer 2 |
| `mur-agent-runtime/src/task_runner.rs` | New `triggered_skill` field; trigger matching |

## 5. Composition & Dependencies

### 5.1 Dependency Declaration

Skills declare dependencies via `requires`:

```yaml
requires:
  - name: web-browsing
    version: ">=1.0.0"
  - name: data-table-export
    version: ">=0.5.0, <2.0.0"
```

### 5.2 Resolution

```
Install research-prices
  -> Check requires
  -> web-browsing installed (version 1.2.0 >= 1.0.0) -> skip
  -> data-table-export not installed -> recursive install
  -> Cycle detection: reject on circular dependencies
  -> Write skill.lock with resolved versions
```

### 5.3 Runtime Composition

Dependencies are composition, not inheritance. When `research-prices` triggers and loads Layer 3:
- Its procedure steps reference dependency skills by trigger
- The runtime loads each dependency's Layer 3 only when its trigger fires
- Dependencies are independent, testable units

### 5.4 Version Constraints

Semver matching. `>=1.0.0`, `^1.2.3`, `~1.2.3`, exact `1.2.3`. No range means `*` (any version). Lock file pins exact versions.

## 6. Registry & Discovery

### 6.1 Dual-Federated Registry

**Primary — Git-based registry** (zero-infrastructure):
```
https://github.com/mur-run/skill-registry
  index.yaml                    # Search index (auto-updated on publish)
  skills/
    research-prices/
      versions/
        1.0.0.yaml
        1.1.0.yaml
    web-browsing/
      ...
```

Publishing = opening a PR to this repo. No server required.

**Secondary — Agent Card broadcast** (decentralized):
- Each agent broadcasts skills in its A2A Agent Card (existing `skills` field)
- Other agents discover skills via `GET /.well-known/agent-card.json`
- Skill content fetched via new A2A endpoints

### 6.2 CLI

```bash
mur skill install research-prices              # From default registry
mur skill install https://github.com/...        # From git URL
mur skill install agent://my-agent              # From another agent
mur skill install ./skill.yaml                  # From local file
mur skill search "prices"                       # Search registry
mur skill search "prices" --local               # Search installed only
mur skill info research-prices                  # Layer 1+2 summary
mur skill info research-prices --full           # Complete skill
mur skill publish ./skill.yaml                  # Push to registry
mur skill update research-prices                # Update to latest
mur skill list                                  # List installed
mur skill remove research-prices                # Uninstall
mur skill validate ./skill.yaml                 # Validate format
mur skill fmt ./skill.yaml --markdown            # Convert to markdown
```

### 6.3 Search Index

`index.yaml` in registry root, regenerated on each publish:

```yaml
skills:
  research-prices:
    latest: 1.1.0
    description: Search and compare product prices
    publisher: human:david
    category: workflow
    tags: [e-commerce, price, shopping]
    downloads: 42
    rating: 4.5
```

Local cache at `~/.mur/cache/registry-index.yaml`, refreshed on `mur skill search`.

## 7. Peer Transfer Protocol

### 7.1 Pull Transfer (primary)

```
Agent A (has skill)                     Agent B (wants skill)

1. DISCOVER
   B reads A's Agent Card
   -> sees skills: ["research-prices", "web-browsing"]

2. REQUEST
   B -> A:  GET /skills/research-prices
   A -> B:  { skill manifest (L1) + abstract (L2) }

3. DECIDE
   B evaluates relevance to own tasks
   -> Yes: request full body
   -> No:  stop

4. TRANSFER
   B -> A:  GET /skills/research-prices?layer=full
   A -> B:  complete skill yaml (L1+L2+L3)

5. INSTALL
   B writes to local skill store
   B registers in profile.yaml
   B optionally re-shares to registry
```

### 7.2 Push Offer (supplementary)

```
A -> B:  POST /skills/offer
         {
           "skill_name": "research-prices",
           "reason": "Detected you are scraping product pages",
           "confidence": 0.85
         }

B -> A:  { "accepted": true }   -> proceed to Transfer step
         { "accepted": false }  -> stop
```

### 7.3 A2A Endpoint Additions

| Endpoint | Method | Response |
|----------|--------|----------|
| `/skills/{name}` | GET | Layer 1 + Layer 2 |
| `/skills/{name}?layer=full` | GET | Complete skill (L1+L2+L3) |
| `/skills/offer` | POST | Accept/decline response |

### 7.4 Provenance Tracking

Each installed skill records its origin:

```yaml
provenance:
  source: agent://research-agent        # Where it came from
  transferred_at: 2026-05-24T10:30:00Z
  original_publisher: agent:research-agent
  transfer_chain: [agent://research-agent]
```

Re-shared skills append to `transfer_chain`, creating an auditable skill propagation graph.

### 7.5 Trust Model (Deferred to M5)

Peer-transferred skills are installed with `priority: low` and a `peer-transferred: true` flag. The runtime may limit their token budget or require human approval before first use. Full trust/safety metadata (signatures, publisher verification, safety ratings) is deferred to a future design.

## 8. Agent-Generated Skills

### 8.1 Generation Triggers

1. **Manual**: `mur skill generate --from-session <session-id>`
2. **Auto-suggest**: Agent detects repeated task pattern >= 3 times, offers to extract
3. **Pattern promotion**: `mur skill from-pattern <pattern-name>` — promote a Stable/Canonical pattern to a skill

### 8.2 Generation Pipeline

```
Session recording (mur in/out)
  -> LLM analyzes session events
  -> Identifies repeatable step sequences
  -> Extracts Procedure:
      - Variables: parameterized parts of the conversation
      - Steps: derived from tool call sequences
      - Tools: derived from actual MCP tool usage
  -> Generates skill.yaml (L1+L2+L3)
  -> Writes to agent skills store
  -> Agent can optionally publish to registry
```

### 8.3 Agent vs Human Output

Agents output canonical YAML. Humans can author in either format. They are equivalent and interconvertible:

```
Canonical YAML (agent output)  <->  Markdown frontmatter (human authoring)
       mur skill fmt --yaml               mur skill fmt --markdown
```

## 9. CLI Surface Summary

### New: Global Skill Management

```
mur skill install <source>          # Install from registry/git/agent/file
mur skill remove <name>             # Uninstall a skill
mur skill list                      # List installed skills
mur skill show <name>               # Display full skill content
mur skill search <query>            # Search registry (+ --local flag)
mur skill info <name>               # Layer 1+2 summary (+ --full flag)
mur skill publish <path>            # Publish to registry
mur skill update <name>             # Update to latest version
mur skill validate <path>           # Validate skill format
mur skill fmt <path> [--markdown|--yaml]  # Convert between formats
mur skill generate --from-session <id>    # Generate skill from session recording
mur skill from-pattern <pattern>    # Promote pattern to skill
```

### Upgraded: Agent Skill Binding

```
mur agent skill add <agent> <skill>     # Bind existing skill to agent
mur agent skill remove <agent> <name>   # Unbind skill from agent
mur agent skill list <agent>            # List agent's skills
mur agent skill show <agent> <name>     # Show skill content
mur agent skill publish <agent> <name>  # Publish agent's skill to registry
```

## 10. Migration Path

### Phase 1 — Format Compatibility (non-breaking)
- `Skill` struct supports parsing from old markdown frontmatter
- Old markdown files auto-treated as `context` mode with Layer 2 only
- `mur skill validate` suggests upgrade but does not error on old format

### Phase 2 — Tooling Conversion
- `mur skill upgrade <name>` interactively converts old markdown to new format
- `mur sync` writes new-format skills to AI tool directories
- Four built-in skills updated to new format

### Phase 3 — Runtime Activation
- Agent runtime injects skill Layer 2 into system prompt
- Trigger matching enabled
- Peer transfer protocol endpoints online

### Built-in Skill Migration

| Current Skill | New Category | Trigger |
|---------------|-------------|---------|
| mur-context | context | session_start |
| mur-in | command | command: `/mur-in` |
| mur-out | command | command: `/mur-out` |
| mur-run | workflow | keyword: `mur run`, `/mur-run` |

## 11. Milestones

### M0 — Foundation
- `Skill` struct in `mur-common/src/skill.rs` with serde + validation
- Dual format parser (canonical YAML + markdown frontmatter)
- `~/.mur/skills/<name>/skill.yaml` storage
- `mur skill validate`
- Four built-in skills upgraded
- Backward-compatible old-format reader

### M1 — CLI + Registry
- `mur skill install/list/show/remove/search/info`
- Git-based registry: `mur-run/skill-registry` repo + index.yaml
- `mur skill publish` (human flow)
- `mur agent skill add/remove/list/show` upgraded, CLI-compatible

### M2 — Runtime Injection
- Agent runtime reads skills, injects Layer 2 into system prompt
- Token budget + priority logic
- Trigger matching engine (command / keyword / session_start)
- Layer 3 on-demand loading

### M3 — Composition + Agent Generation
- `requires:` dependency resolution and installation
- `skill.lock` lock file
- Circular dependency detection
- `mur skill generate --from-session`
- `mur skill from-pattern`

### M4 — Peer Transfer
- A2A endpoints: `GET /skills/{name}`, `POST /skills/offer`
- Agent Card skill broadcast (existing field, upgraded content)
- Pull transfer flow + push offer flow
- Provenance recording
- `mur skill install agent://<name>`

### M5 — Polish & Ecosystem
- Skill propagation graph visualization
- Registry web UI
- Skill ratings / usage statistics
- CI auto-validation for registry PRs

## 12. Deferred to Future Design

- MCP tool binding within skills (user deselected in brainstorming)
- Trust/safety metadata: signatures, publisher verification, safety ratings (user deselected)
- Skill execution sandboxing for peer-transferred skills
- Paid/private skill registries
- Cross-platform skill compatibility matrix
