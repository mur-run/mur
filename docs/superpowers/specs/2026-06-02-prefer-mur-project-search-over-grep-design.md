# Design: Make Claude Code prefer `mur project search` over grep

**Date:** 2026-06-02
**Status:** Draft (awaiting review)
**Topic:** Steer Claude Code toward semantic codebase search where it helps, while keeping grep authoritative where it must be.

## Problem

When using Claude Code in this repo, Claude reaches for the built-in `Grep`
tool for every code search, never `mur project search` — even though semantic
(hybrid vector + BM25) search would answer concept/intent queries better and
more cheaply.

Root cause (verified):

1. **The MCP server is not connected.** `~/.claude/settings.json` has
   `ENABLE_CLAUDEAI_MCP_SERVERS: false`, there is no `.mcp.json`, and `mur` is
   absent from the MCP tool list. The `mur_project_search` tool
   (`mur-mcp-server/src/tools.rs:77`) therefore is not reachable.
2. **No skill teaches Claude to choose it.** The mur skills loaded at session
   start are `mur-project-index` and `mur-project-remove` only — both about
   *building/removing* the index, neither about *searching*. There is no
   `mur-project-search` skill.
3. **No hook redirects search intent.** `on-tool.sh` captures/filters; it does
   not rewrite `grep` → semantic search (and should not — see below).

`Grep` is a first-class Claude Code tool, always available and the natural
default. `mur project search` is only reachable via a Bash CLI call or the
(disconnected) MCP tool. So Claude always falls back to grep.

## Decision: complementary, not replacement

We explicitly **reject** "fully replace grep" and **reject** hook-rewriting of
grep calls. Two correctness hazards make replacement wrong, not merely
sub-optimal:

- **Freshness.** `grep` reads the working tree as it is *right now*, including
  code Claude wrote seconds ago. `mur project search` reads an *index* that is
  only as fresh as the last `mur project index`. In an active coding session,
  relying on semantic search to find just-edited, un-indexed code yields false
  negatives — Claude may conclude code "does not exist."
- **Completeness.** Refactors (rename, find-all-callers, dead-path removal)
  need *exhaustive, exact* matches. Semantic search returns ranked top-k
  "most relevant" snippets by design — it does not guarantee every occurrence.
  Missing a call site breaks the build.

Hook-rewriting is rejected for the same reasons: a grep pattern alone cannot be
classified as "exact symbol" vs "concept" (e.g. `handle.*payment` is both), so
blunt rewriting would hit both hazards.

**Adopted posture:** the decision *rule* is "right tool per intent" (complementary),
with a *conservative default* — grep stays the default and the authoritative
source; semantic search is the deliberate choice for fuzzy/conceptual queries.

| Query shape | Tool |
|---|---|
| Concept / intent ("where is the logic that handles X") | `mur project search` |
| Exact symbol / string / import / config key | **grep** |
| Needs exhaustive results (rename, all callers) | **grep** |
| Code edited this session, not yet indexed | **grep** (hard rule) |
| Index reported stale by `mur project status` | **grep** (fall back) |

Token note: for concept queries, one semantic call returning ~5 ranked
`file:line` snippets is typically cheaper than grep returning dozens of matches
that Claude must then open files to understand. The split above aligns with
token economy, not just correctness.

## Components

Three pieces. One is already implemented; the work is the other two.

### 1. Connect the mur MCP server  *(new)*

Register the `mur-mcp-server` binary (stdio JSON-RPC; see
`docs/superpowers/specs/2026-06-01-mur-mcp-server-and-skills-design.md`) so
`mur_project_search` becomes a first-class tool alongside `Grep`.

- Add a project-scoped `.mcp.json` at the repo root that launches the binary.
  Command resolves to the installed binary (e.g. `~/.mur/bin/mur-mcp-server`
  or the workspace `target` build); exact path/launcher to be pinned during
  implementation, following the launch contract in the MCP design spec.
- This is *necessary infrastructure*: without it, semantic search is only
  reachable by shelling out via Bash, which Claude will not naturally choose.
- Exposure alone does **not** change behavior — Claude still defaults to grep
  until Component 2 guides the choice.

### 2. Guiding skill `mur-project-search`  *(new — the missing piece)*

A skill that encodes the decision rule so the model actually chooses correctly.

- **Triggers:** keyword/intent patterns for conceptual search, e.g.
  "where is the code that…", "how does X work", "find the logic responsible
  for…", "which file handles…". Plus manual.
- **Body teaches:**
  - Concept/intent query → call `mur_project_search` (MCP) or
    `mur project search "<query>"` (CLI fallback).
  - Exact symbol/string, imports, config keys, or any exhaustive search
    (rename, all callers) → use `Grep`.
  - **Hard rule:** code created/modified in the current session and not yet
    indexed → use `Grep` (semantic index always lags un-committed edits).
  - Before trusting semantic results, if `mur project status` reports the
    index stale/missing → fall back to `Grep`.
- Mirrors the existing `mur-project-index` skill format (frontmatter with
  `triggers`, `hosts`, `priority`, builtin tag).

### 3. Freshness via git post-commit hook  *(largely already implemented)*

`codebase::ensure_git_hook` (`mur-core/src/codebase/mod.rs:707`) already
implements the agreed design:

- Marker `# mur auto-index` makes it **idempotent** (skips if present).
- Creates `.git/hooks/post-commit` with shebang + `0o755` if absent;
  **appends a guarded block** if a hook already exists (never clobbers).
- Hook runs `mur project index "<path>" --quiet --background` — incremental
  (mtime cache), background, and a no-op guarded by `command -v`.
- Invoked automatically after a foreground `mur project index`
  (`mur-core/src/cmd/project.rs:333`).

**Gap to fix:** the **background-index worker**
(`cmd_project_index_worker`, `mur-core/src/cmd/project.rs`) does **not** call
`ensure_git_hook`. A large project whose *first* index runs via `--background`
(auto-detected when chunks exceed the background threshold) will silently skip
hook installation. Fix: call `ensure_git_hook` from the background worker path
too, so first-index hook installation has parity regardless of fg/bg mode.

This satisfies the agreed decisions:
- **(a) Install timing:** hook is installed automatically on first successful
  index (after the gap fix, in both fg and bg paths) — no separate command to
  remember.
- **(b) Pre-existing hook:** append a marker-delimited block, idempotent, never
  overwrite.

## Out of scope

- Hook-rewriting `Grep` calls into semantic search (rejected above).
- A always-on file-watching daemon for instant reindex (option C; deferred —
  cost outweighs benefit, and it still cannot cover un-saved edits).
- Changes to retrieval quality / ranking of `mur project search` itself.
- Tuning the MCP server's other tools.

## Testing

- **Component 1:** spawn `mur-mcp-server`, `tools/list` includes
  `mur_project_search`; calling it on an indexed repo returns ranked
  `file:line` snippets. Verify Claude Code lists the `mur` MCP server after
  adding `.mcp.json`.
- **Component 2:** skill loads at session start (appears in mur learning
  index); a concept-style prompt triggers it; an exact-symbol prompt does not
  divert from grep.
- **Component 3:** unit/integration test that `ensure_git_hook` is invoked from
  the background worker path; existing idempotency/append behavior covered by a
  test that running it twice yields one marker block and that a pre-existing
  hook is preserved.

## Open implementation details (pin during plan)

- Exact `.mcp.json` launch command and binary path resolution for
  `mur-mcp-server` (installed vs workspace build).
- Whether the `mur-project-search` skill ships as a builtin (alongside
  `mur-project-index`) so it auto-installs for all users, vs project-local.
