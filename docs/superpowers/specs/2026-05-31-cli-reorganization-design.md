# CLI Reorganization Design

**Date:** 2026-05-31
**Status:** design-approved

## Problem

The `mur` CLI has 45 top-level commands — overwhelming for users. Several are dead code, outdated, or conceptually overlapping. There's no clear hierarchy: high-frequency daily commands sit alongside low-level internals with no visual grouping.

Specific issues:

1. **Dead code:** `mur gc` (stub), `mur import` (deprecated message only)
2. **Outdated descriptions:** `mur search` still says "Search patterns" but the codebase is migrating Pattern → Skill
3. **Unnecessary aliases:** `mur exit` / `mur quit` duplicate each other; behavior (stop session + delete recording) should be a `mur session` subcommand
4. **Flat login/logout:** `mur login` / `mur logout` are rarely typed; grouping under `mur auth` reduces top-level noise
5. **Conceptual overlap:** `mur chat` + `mur conversations`, `mur inject` + `mur context`, `mur suggest` + `mur skill suggest`
6. **Strategy conflict:** `mur community` is a public marketplace — contradicts the "NO marketplace" strategy; functionality already covered by `mur skill publish`/`mur skill install`
7. **Scattered daemon commands:** `murmurd`, `serve`, `sleep` are all daemon-related but spread across top-level

## Design

### Philosophy

Git-style noun-verb grouping. Top-level ≈ 11 noun groups + 12 standalone commands. Commands that share a concept (agent, skill, session, sync, daemon) live under a common parent. Standalone commands justify their top-level slot by being conceptually unique — they don't naturally belong under any noun group.

### Final CLI Structure

#### Noun Groups (11)

```
mur agent          # Agent lifecycle (unchanged, 35 subcommands)
mur skill          # Skill management (+ exchange, drafts, eval)
mur notes          # Notes: category:note skills (+ search)
mur session        # Session recording (in, out, discard)
mur model          # Model registry (unchanged)
mur auth           # Authentication (login, logout) — NEW group
mur hook           # AI tool hooks (+ inject, context)
mur sync           # Tool sync + fleet sync
mur workflow       # Workflows (+ run, suggest)
mur chat           # Conversation archive (+ conversations, ask)
mur daemon         # Background services (murmurd + sleep + serve)
```

#### Standalone (12)

```
mur init           # First-run initialization
mur doctor         # Health check (updated: skills, not patterns)
mur verify         # Documentation claims verification
mur update         # Self-update
mur stats          # Statistics and effectiveness
mur project        # Project source indexing
mur team           # Team sharing (private, enterprise)
mur source         # External knowledge sources (feature-gated)
mur internals      # Low-level internal ops (+ reindex)
mur push           # Push outbox signals → server
mur fetch          # Pull inbox signals ← server
mur dashboard      # Terminal TUI dashboard
mur deploy         # Docker Compose deployment
```

### Migration Map

| Old Command | New Command | Notes |
|---|---|---|
| `mur search` | `mur notes search` | Updated to search notes, not patterns |
| `mur exit` | `mur session discard` | Stop session + delete recording |
| `mur quit` | `mur session discard` | Alias; same behavior |
| `mur in` | `mur session in` | Shortcut for session start + context |
| `mur out` | `mur session out` | Shortcut for session stop + menu |
| `mur login` | `mur auth login` | |
| `mur logout` | `mur auth logout` | |
| `mur inject` | `mur hook inject` | Manual injection test |
| `mur context` | `mur hook context` | Context-aware injection |
| `mur run` | `mur workflow run` | |
| `mur suggest` | `mur workflow suggest` | |
| `mur murmurd` | `mur daemon` | `murmurd start` → `mur daemon start` |
| `mur serve` | `mur daemon serve` | Web dashboard API server |
| `mur sleep` | `mur daemon sleep` | |
| `mur conversations` | `mur chat` subcommands | `conversations pull` → `mur chat pull` |
| `mur ask` | `mur chat ask` | |
| `mur exchange` | `mur skill exchange` | |
| `mur drafts` | `mur skill drafts` | |
| `mur eval` | `mur skill eval` | |
| `mur reindex` | `mur internals reindex` | Rebuild LanceDB vector index |
| `mur gc` | **REMOVED** | Dead stub; use `mur skill sweep` |
| `mur import` | **REMOVED** | Dead code; use `mur notes ingest` |
| `mur community` | **REMOVED** | Strategy conflict; use `mur skill publish/install` |

### Number Change

```
Top-level: 45 → 23 (11 groups + 12 standalone)
Actual entry points (including subcommands): unchanged
```

## Implementation Phases

### Phase 1: Remove Dead Code

Delete CLI definitions and dispatch branches for:
- `mur gc` — stub pointing to `mur skill sweep`
- `mur import` — deprecated message
- `mur community` — all 8 subcommands (publish, fetch, search, list, star, report, packs, pack)

Also remove community command implementation files and any community-specific server API code that is now dead.

### Phase 2: New Group Scaffolding

Add new `Commands` enum variants and sub-action enums:
- `Commands::Auth(AuthAction)` — `Login`, `Logout`
- `Commands::Daemon(DaemonAction)` — `Start`, `Stop`, `Status`, `Serve`, `Sleep(SleepAction)`

Update `mur daemon` to wrap the existing `murmurd` runtime + `serve` + `sleep` dispatch.

### Phase 3: Move Commands

For each command being moved, add the new CLI definition and dispatch while keeping the old one (hidden, with deprecation warning):

1. `mur search` → add `NotesAction::Search`, old `Commands::Search` hidden + warns
2. `mur exit`/`quit` → add `SessionAction::Discard`, old aliases hidden + warn
3. `mur in`/`out` → add `SessionAction::In`/`Out`
4. `mur login`/`logout` → move under `AuthAction`
5. `mur inject`/`context` → move under `HookAction`
6. `mur run`/`suggest` → move under `WorkflowAction`
7. `mur murmurd` → `DaemonAction` (start/stop/status)
8. `mur serve` → `DaemonAction::Serve`
9. `mur sleep` → `DaemonAction::Sleep`
10. `mur conversations` → merge into `ChatAction`
11. `mur ask` → `ChatAction::Ask`
12. `mur exchange` → add to `SkillAction`
13. `mur drafts` → add to `SkillAction`
14. `mur eval` → add to `SkillAction`
15. `mur reindex` → add to `InternalsAction`

### Phase 4: Cross-Project Reference Update

Scan and update all references to old command names in:

1. **mur repo:**
   - CLI definition files (`mur-core/src/cli/`)
   - Command implementations (`mur-core/src/cmd/`)
   - Dispatch (`mur-core/src/dispatch.rs`)
   - Skill definitions that invoke mur CLI commands
   - Workflow YAML files
   - Documentation (`docs/`, `README.md`)
   - Shell scripts (`install.sh`, `build.sh`)
   - CI configs (`.github/workflows/`)
   - Tests

2. **mur-commander repo:**
   - All skill files that invoke `mur in`, `mur out`, `mur search`, `mur exit`, `mur quit`, `mur run`, `mur suggest`
   - Any hook configurations
   - Documentation references

### Phase 5: Update Descriptions and Internals

- `mur doctor`: update to check skill health instead of pattern count
- `mur stats`: update to aggregate skill statistics instead of pattern statistics
- `mur notes search`: ensure it searches notes (category:note skills), not patterns
- Update all CLI `///` doc strings that reference "patterns" to use "skills" or "notes" as appropriate

### Phase 6: Cleanup (Future)

After a transition period (1-2 releases), remove all hidden/deprecated old commands from the CLI definition entirely.

## Out of Scope

- Pattern → Skill Tier C2 retirement (separate plan: `docs/superpowers/plans/2026-05-31-pattern-skill-tierc-retirement.md`)
- Adding new subcommands beyond what's needed for the move
- Changing command behavior (only routing and descriptions)
- `mur config` command (future feature, not part of this reorganization)

## Compatibility Notes

- All old command names emit a deprecation warning for one release cycle before removal
- `mur exit`/`mur quit` — the `session discard` behavior (stop + delete recording) is preserved
- `mur murmurd` binary path is unchanged; only the CLI routing changes
