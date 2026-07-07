# Agent Output Locations — Route Authored Artifacts into `~/.mur`

**Date:** 2026-07-07
**Status:** Design
**Scope:** Guidance-only (prompt + skill), no `mur-agent-runtime` changes.

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

- No runtime enforcement (no changes to fs entitlements or default
  `working_dir`). This is guidance the agent follows; if it proves
  insufficient, a runtime guardrail is a separate follow-up.
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

Two documentation/prompt edits, no code paths:

1. **Concierge `sys_prompt.md`** and the **`mur agent create` default prompt
   template** — add a short "Output locations" section stating the three
   rules. Editing the template means every newly-created agent inherits the
   rule, not just the concierge.
2. **`mur-workflow-author` skill** (and the skill-authoring guidance) — carry
   the same rule so the authoring flow itself points at `~/.mur`.

### Cleanup for the current instance

- Install the stray draft properly:
  `mur skill install registry-work/skills/remote-security-scan/1.0.0/skill.yaml`
  so it appears in `~/.mur/skills/` and the Hub.
- Add `registry-work/` to `.gitignore` — it is a repo scratch directory
  (defined by `docs/superpowers/plans/2026-07-04-role-skill-packs.md`) and
  should never be committed.

## Verification

Guidance changes are verified by behavior, not tests:

1. In `murmur`, ask the agent to build a workflow → its `skill.yaml` lands in
   `~/.mur/skills/` (confirmed via `mur skill list`), not the source tree.
2. Run a workflow that emits a report → the report lands in
   `~/.mur/artifacts/<agent-name>/…`, not the working directory.
3. `git status` in the repo stays clean of agent-authored files.

## Risks

- **Self-discipline only.** A guidance rule can be ignored by the model. This
  is the accepted trade-off for a small, immediate fix; escalate to a runtime
  guardrail only if drift is observed in practice.
