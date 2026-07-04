# Role Skill Packs Implementation Plan (3/4)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fleet role agents get methodology skill packs (superpowers derivatives) installable as one action, tagged by role in the registry.

**Architecture:** Port nine superpowers skills to MUR skills, publish under `mur-official`, add `recommended_roles` to the registry entry model, and add `mur agent skill install-pack <role>` (the Hub wizard consumes the same engine later). Spec §D. **Depends on Plan 1 (origin pipeline) and Plan 2 Task 4 (registry publishing pattern).**

**Tech Stack:** MUR skill YAML, `mur-common/src/skill/registry.rs`, `mur-core/src/cmd/agent/skill.rs`.

## Global Constraints

- Same as Plan 1 (nextest, ORT_STRATEGY, fmt+clippy).
- Ports are rewrites for the MUR runtime: replace "dispatch subagent" with fleet delegation or sequential execution; replace Claude-Code tool names with the agent runtime's tools (bash, file ops); strip plan-mode/Skill-tool/TodoWrite references. Attribution: "Derived from obra/superpowers <name> (MIT)."
- All published skills are canonical-unstamped in the registry; origin stamps apply at install (Plan 1 Task 2).

## Skill → Role Map (source: superpowers 6.1.1 plugin cache)

| Registry name | Source skill | recommended_roles | Port notes |
|---|---|---|---|
| brainstorming | brainstorming | concierge | done in Plan 2 |
| writing-plans | writing-plans | pm | terminal state = write plan file + report path to router; consumes brainstorming's design summary |
| executing-plans | executing-plans | coder | batch/checkpoint execution of a plan file; no subagents — sequential tasks |
| test-driven-development | test-driven-development | coder | keep RED-GREEN-REFACTOR verbatim; map commands to `cargo nextest` example |
| using-git-worktrees | using-git-worktrees | coder | worktrees under repo `.worktrees/` (project convention), never `~/.mur` |
| systematic-debugging | systematic-debugging | coder | phase-gated root-cause process; drop subagent dispatch |
| verification-before-completion | verification-before-completion | coder | "run the verification command and read its output before claiming done" |
| receiving-code-review | receiving-code-review | coder | verify feedback technically before applying; no performative agreement |
| code-review | requesting-code-review | qa | **semantics inverted**: qa IS the reviewer — how to review a diff against a plan/spec and report severity-ranked findings |
| finishing-a-development-branch | finishing-a-development-branch | repo | merge/PR decision flow; merge-commit for stacked bases (project convention) |

---

### Task 1: `recommended_roles` on the registry entry

**Files:**
- Modify: `mur-common/src/skill/registry.rs` (`RegistrySkillEntry`)
- Test: existing registry tests in the same file/module

- [ ] **Step 1: Failing test** — parse an `index.yaml` snippet with `recommended_roles: [coder, qa]` into `RegistrySkillEntry` and assert the vec; assert an entry without the key parses to `[]` (back-compat).
- [ ] **Step 2:** Run `cargo nextest run -p mur-common registry` → fails.
- [ ] **Step 3:** Add to `RegistrySkillEntry`:

```rust
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub recommended_roles: Vec<String>,
```

Role vocabulary (validate loosely, don't enum-lock the registry format): `concierge|pm|coder|qa|repo`.
- [ ] **Step 4:** Tests pass; commit `feat(skill): recommended_roles on registry entries`.

### Task 2: Port the nine skills

One sub-step per skill, same recipe as Plan 2 Task 1 (read source SKILL.md from the superpowers 6.1.1 plugin cache, rewrite per the Port-notes column, `version: 1.0.0`, `publisher: mur-official`, validate with `mur_common::skill::validate`). Work in a scratch dir `registry-work/skills/<name>/1.0.0/skill.yaml`.

- [ ] writing-plans
- [ ] executing-plans
- [ ] test-driven-development
- [ ] using-git-worktrees
- [ ] systematic-debugging
- [ ] verification-before-completion
- [ ] receiving-code-review
- [ ] code-review (inverted)
- [ ] finishing-a-development-branch
- [ ] **Validation gate:** a small `#[test]` (temporary, or a `tests/` file in mur-common pointed at the scratch dir via env) that `read_from_dir` + `validate` passes for all nine. Alternative: `mur agent skill install --file <dir> --dry-run` if that exists — check `mur agent skill --help` first.
- [ ] Commit the scratch dir? No — registry files live in the registry repo (Task 4). Nothing in this repo to commit for this task.

### Task 3: `mur agent skill install-pack <role> --agent <name>`

**Files:**
- Modify: `mur-core/src/cmd/agent/skill.rs` (new subcommand beside the existing install)
- Test: `#[cfg(test)]` in the same file

**Interfaces:**
- Consumes: `skill_registry::{fetch_and_load}`, Task 1's `recommended_roles`, the existing single-skill registry install path (which stamps origin per Plan 1 Task 2).
- Produces: installs every registry skill whose `recommended_roles` contains `<role>` into the agent, skipping ones already present (report `installed: [...] skipped: [...]`).

- [ ] **Step 1: Failing test** — with a fake registry index (three entries: two `coder`, one `qa`), `pack_members(&index, "coder")` returns exactly the two coder names, sorted.
- [ ] **Step 2:** fails to compile.
- [ ] **Step 3:** Implement `pack_members` (pure filter) + the subcommand loop reusing the existing install fn per name; already-installed check = `agent_skill_dir(...).join(name).exists()`.
- [ ] **Step 4:** `cargo nextest run -p mur-core pack` PASS; clippy clean.
- [ ] **Step 5:** Commit `feat(cli): mur agent skill install-pack <role>`.

### Task 4: Publish + wire the fleet

- [ ] Publish all nine to the registry repo (same recipe as Plan 2 Task 4) with `recommended_roles` in `index.yaml`.
- [ ] Dogfood on this machine: `mur agent skill install-pack coder --agent rustsmith`, `install-pack coder --agent frontend`, `install-pack qa --agent qa`, `install-pack repo --agent repomanager`, `install-pack pm --agent pm`. Verify: `mur agent skill list <agent>` shows the pack; `mur skill upgrade --check` reports all `UpToDate`.
- [ ] Restart the affected runtimes (never auto-restart — `mur agent restart` per agent, concierge is Hub-managed).

## Deferred

- Hub agent-creation-wizard pack picker UI (consumes `install-pack` engine; fold into the next Hub UI batch).
- Dashboard role filtering (Plan 4's surface).
