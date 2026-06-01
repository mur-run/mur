# Agent Skill Anthropic Layout — Design

**Date:** 2026-06-02
**Status:** Approved (design), pending implementation plan
**Scope:** Per-agent skill storage only (`~/.mur/agents/<agent>/skills/`). The global
skill layer (`~/.mur/skills/<name>/`, written by `sync_cmd`) is already
Anthropic-compliant and is out of scope.

## Problem

Per-agent skills are installed as a **flat file** instead of the Anthropic
skill-package directory format. Observed on disk for agent `Author`:

```
~/.mur/agents/Author/skills/technical-writing.md          # actual (wrong)
```
registered in `profile.yaml` as `skills/technical-writing.md`.

The file *content* is already valid Anthropic format (frontmatter with only
`name` + `description`, followed by a markdown body). Only the **placement** is
wrong. The correct Anthropic layout is a directory whose entry point is the
upper-case `SKILL.md`, so the package can also ship supporting resources:

```
~/.mur/agents/Author/skills/technical-writing/
├── SKILL.md          # entry point (name + description frontmatter + body)
├── scripts/          # optional executable helpers
└── references/       # optional reference material
```

### Root cause

`mur-core/src/cmd/agent/skill.rs::cmd_skill_add` copies the source file directly
to `skills/<basename>` (`skill.rs:73`, `skills_dir.join(basename)`). It never
creates a directory and silently drops any `scripts/` / `references/` that a
package ships.

### Why it currently "works" anyway

The runtime loader reaches the flat file only through a **legacy fallback** in
`mur-common/src/skill/store.rs::read_from_dir` (`dir.with_extension("md")`,
parsed by the pre-M0 `parse_legacy_markdown`). It loads by luck of the deprecated
path, not by design, and that path cannot carry supporting resources.

## Decisions (best practice)

| Concern | Decision | Rationale |
|---|---|---|
| Source of truth | **`SKILL.md` as-authored is canonical; do NOT convert to `skill.yaml`** | That is exactly what skill packages ship and what is already on disk; avoids mutating author content. |
| Install source forms | **Accept both a single `.md` file and a directory** | Single file → wrap into `<name>/SKILL.md`. Directory → recursively copy the whole package (SKILL.md + scripts/ + references/ + assets/). |
| On-disk entry filename | **Upper-case `SKILL.md`** | Anthropic official convention; matches the global layer `~/.mur/skills/<name>/SKILL.md`. |
| `profile.yaml` registration | **Store the skill *name*** (`technical-writing`), not a file path | Aligns with the runtime loader, which identifies skills by directory name (`local::list_installed_agent` scans directories). |

## Affected components

### 1. Install — `mur-core/src/cmd/agent/skill.rs::cmd_skill_add`
- Resolve a skill `<name>` from the source (file stem, or directory name).
- Source is a `.md` file → create `skills/<name>/` and copy the file in as
  `SKILL.md`.
- Source is a directory → require a top-level `SKILL.md`; recursively copy the
  whole directory tree to `skills/<name>/`.
- Register `<name>` in `profile.skills` (was `skills/<basename>`).
- Update `resolve_skill_id`, `cmd_skill_show`, `cmd_skill_remove` to the new
  name/directory semantics. `remove` uses `remove_dir_all` on `skills/<name>/`.

### 2. Loader read precedence — `mur-common/src/skill/store.rs::read_from_dir`
Extend the lookup order to recognize the Anthropic entry file first:

```
SKILL.md  →  skill.yaml  →  skill.md  →  legacy flat <name>.md
```

`SKILL.md` is required explicitly because case-sensitive filesystems (Linux) do
not match `skill.md` against `SKILL.md`; macOS case-insensitivity must not be
relied upon.

### 3. Lenient `SKILL.md` parsing
Anthropic frontmatter carries only `name` + `description` + body. Parse it with
the lenient defaulting already used by `parse_legacy_markdown`:
- `content.abstract` ← `description`
- `content.context` ← markdown body
- supply sane defaults for absent `category` / `version` / `publisher`

so an Anthropic-minimal `SKILL.md` loads without requiring MUR-specific fields.

### 4. Export / Import recursion
- `mur-agent-runtime/src/export/pkg.rs` (≈L97) and
  `mur-agent-runtime/src/import.rs` (≈L98) currently iterate `skills/` and
  `fs::copy` individual **files**, which skips subdirectories.
- Change both to **recursively copy each skill directory**, so exported and
  imported `.muragent` packages carry `scripts/` and `references/`.

### 5. Migration of existing flat skills
- Migrate `skills/<name>.md` → `skills/<name>/SKILL.md`, idempotent.
- Run lazily on `mur agent skill add` and `mur agent skill list`, and expose an
  explicit fix in `mur agent doctor`.
- Rewrite any stale `profile.skills` entries of the form `skills/<name>.md` to
  the bare `<name>`.
- Result: `Author`'s `technical-writing.md` is upgraded in place to
  `technical-writing/SKILL.md`.

## Out of scope
- Global `sync_cmd` skill layer — already correct.
- Runtime injection semantics (e.g. the `SessionStart` trigger filter in
  `inject_layer2`). This change only fixes on-disk layout, read precedence, and
  package transport; it does not alter when/how skills are injected.

## Acceptance criteria
1. `mur agent skill add <file.md>` produces `skills/<name>/SKILL.md`.
2. `mur agent skill add <dir/>` recursively installs a package containing
   `SKILL.md`, `scripts/`, `references/`.
3. The runtime loader loads a skill from `skills/<name>/SKILL.md`.
4. `mur agent skill remove <name>` deletes the whole `skills/<name>/` directory.
5. Exporting then importing an agent preserves `scripts/` and `references/`.
6. An existing flat `skills/<name>.md` is migrated to `skills/<name>/SKILL.md`
   and its `profile.skills` entry normalized to `<name>`.
7. `profile.skills` stores skill names, not file paths.
