# Agent Output Locations — Route Authored Artifacts into `~/.mur`

**Date:** 2026-07-07
**Status:** Design
**Scope:** Guidance (not enforcement), injected once in the runtime so every
agent inherits it.

## Problem

When a user runs `murmur` (the agent CLI TUI) inside a project directory and
asks the agent to "build a workflow", the produced artifact lands in the
agent's current working directory — which is often a source tree — instead of
a MUR-readable location. Concretely, a `/brainstorming` session that authored
a security-scan skill wrote it to `registry-work/skills/remote-security-scan/
1.0.0/skill.yaml` in the repo root. It was never installed into `~/.mur`,
so `mur skill list`, the retrieval/injection pool, and the Hub could not see
it, and it silently polluted the checkout.

Root cause: the agent's file tools (`write_file` / `edit_file` / `bash`)
resolve relative paths against `working_dir`. When `working_dir` is a repo,
newly-authored files land there. Nothing tells the agent that MUR knowledge
objects belong in `~/.mur` and that run artifacts do not belong in the source
tree. The proper authoring commands (`mur skill install`, `mur workflow new`)
exist but the agent is not directed to use them.

## Goal

Every file an agent *produces* lands where it belongs:

- **Knowledge objects** (workflow / skill / note) → registered under `~/.mur`
  so MUR + Hub see them.
- **Run artifacts** (reports, quarantined files, scratch) → a findable
  per-agent directory, never the source tree.
- **Editing an existing repo file** stays the only reason to write into the
  working directory, and only under the existing consent grant.

## Non-Goals

- No *enforcement*: fs entitlements and the default `working_dir` are
  unchanged. The rule is injected text the agent chooses to follow, not a
  filesystem guardrail; if drift is observed, a runtime guardrail is a
  separate follow-up.
- No new CLI commands. We reuse existing authoring/registration commands.

## Design

### The three output-location rules

| Produced | Destination | How |
| --- | --- | --- |
| Knowledge object (workflow / skill / note) | `~/.mur/{workflows,skills}/` | Register with the real command: skill → `mur skill install <path>`; workflow → `mur workflow new` (or write `~/.mur/workflows/<name>.yaml`). Never leave the definition in the working directory. |
| Run artifact (report, quarantined file, scratch) | `~/.mur/artifacts/<agent-name>/<run>/` | Default here; never the working directory. |
| Edit to an existing repo file | working directory (cwd) | The only case that touches source; requires the existing consent grant (`access::ensure_cwd_access`). |

`<agent-name>` is the agent's own canonical name (it knows this). `<run>` is a
short run label (e.g. a timestamp or task slug) chosen by the agent so
successive runs don't clobber each other.

### Why these commands

- `mur skill install <path>` copies a skill into `~/.mur/skills/<name>/` and
  registers it (reindex with `mur skill reindex-vec` if vector search is
  needed). Direct-writing the YAML would make it exist but can miss the
  vector index — using the command is the robust path.
- `mur workflow new` creates a workflow under `~/.mur/workflows/`.

### Where the rule lives

Editing individual prompt files is too weak: the `mur agent create` default
prompt is a bare `"You are an assistant."` stub, so a prompt edit reaches only
new agents; editing the concierge's `~/.mur/agents/mur/sys_prompt.md` is a
local, per-machine change that never ships. Instead inject the rule **once, in
the runtime**, so every agent inherits it and it ships in the binary:

1. **`mur-agent-runtime` `assemble_system_prompt`** (`task_runner.rs`) — append
   a constant "Output locations" block to `base` (the agent's own system
   prompt) on every turn. One `const`, one append. This is injection, not
   enforcement — the agent still writes files itself; it now always knows
   where they belong.
2. **`mur-workflow-author` skill** (`~/.mur/skills/mur-workflow-author/
   skill.yaml`, and the repo-tracked source if one exists) — carry the same
   rule so the authoring flow itself points at `~/.mur`.

### Cleanup for the current instance

- Install the stray draft properly:
  `mur skill install registry-work/skills/remote-security-scan/1.0.0/skill.yaml`
  so it appears in `~/.mur/skills/` and the Hub.
- Add `registry-work/` to `.gitignore` — it is a repo scratch directory
  (defined by `docs/superpowers/plans/2026-07-04-role-skill-packs.md`) and
  should never be committed.

## Verification

A unit test on `assemble_system_prompt` asserts the block is present in the
assembled prompt. Behavior is then verified end-to-end:

1. In `murmur`, ask the agent to build a workflow → its `skill.yaml` lands in
   `~/.mur/skills/` (confirmed via `mur skill list`), not the source tree.
2. Run a workflow that emits a report → the report lands in
   `~/.mur/artifacts/<agent-name>/…`, not the working directory.
3. `git status` in the repo stays clean of agent-authored files.

## Risks

- **Self-discipline only.** A guidance rule can be ignored by the model. This
  is the accepted trade-off for a small, immediate fix; escalate to a runtime
  guardrail only if drift is observed in practice.
