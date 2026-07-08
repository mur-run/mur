# Skill Bundled Assets — Design

**Date:** 2026-07-08
**Status:** Design (approved for planning)
**Approach:** A — "skill is a directory + resolved path"

## Problem

MUR flattens an imported skill down to a single `skill.yaml`. When a skill
ships sibling files — e.g. obra/superpowers `brainstorming` carries
`scripts/frame-template.html`, `scripts/helper.js`, `scripts/server.cjs`,
`scripts/start-server.sh`, `scripts/stop-server.sh` — those files are **dropped
at import**. The installed skill is a lone `skill.yaml`, so any instruction in
the skill body that runs a bundled script (`bash scripts/start-server.sh`) has
nothing to resolve against.

Three facts cause this, across three layers:

- **Import** (`mur-core/src/cmd/agent/addon/import.rs`): reads only `SKILL.md`
  per skill dir; every sibling is ignored.
- **Storage:** a skill installs to `~/.mur/agents/<agent>/skills/<name>/skill.yaml`
  — one file, no co-located assets.
- **Runtime:** skill content is injected as prompt text
  (`mur-agent-runtime/src/skills/{injector,trigger_matcher}.rs`). The agent is
  never told where its skill lives on disk, so no path resolves.

## Goal

A skill can ship sibling files; the running agent can **read and execute** them
using its existing file and bash tools. This delivers full superpowers parity
for the case that matters — the concierge `mur` agent, which runs on the user's
Mac where a human and a browser are present.

## Non-goals

- **No browser-forwarding.** A headless agent (remote / fleet) will *run* a
  bundled script and bind localhost, but no human sees a served page. This is a
  documented limitation, not a defect. Fixing it would require a real
  browser-forwarding subsystem and is explicitly out of scope.
- **No new `mur agent skill run <name> <script>` verb.** Execution goes through
  the agent's existing bash tool, not a new CLI surface.
- **No new sandbox or entitlement concept** beyond the tool entitlements the
  agent already runs under.

## Approach A — why this is small

"Full parity" sounds like "build a script-execution subsystem." It is not. A
MUR agent already has:

- a **bash tool with a per-call `cwd`** (the same mechanism fleet worktree
  execution uses),
- **node/bash on the host**,
- an **import-time trust scan** (`scan_or_block`) and **bundle signing / TOFU**
  (`skill_bundle`, `skill_verify`).

The only missing facts are (1) the bundle files are discarded and (2) the agent
doesn't know its skill's directory. Fix those two and parity falls out. No new
subsystem, no new security model.

## Design

### 1. Storage — no new format

A skill is already a directory. Bundled files live **beside** `skill.yaml` in
that same directory, preserving the source layout:

```
~/.mur/agents/<agent>/skills/<name>/
  skill.yaml            # content: source of truth (unchanged)
  scripts/
    start-server.sh
    server.cjs
    helper.js
    frame-template.html
    stop-server.sh
```

No manifest field, no asset registry, no separate store. `skill.yaml` remains
the single source of truth for skill *content*; the directory is the *bundle*.

### 2. Import — preserve siblings

In `addon/import.rs`, after `skill_md_to_manifest` produces the manifest and
`scan_or_block` clears it, recursively copy every entry in the **source** skill
directory except `SKILL.md` into the install destination.

- One recursive copy into `dest`.
- **Path-safety check (new):** reject any bundle entry that would escape
  `dest` — path components containing `..`, absolute paths, or symlinks that
  resolve outside `dest`. Reuse the intent of `safe_member_name`. On a rejected
  entry, fail the import with a clear error rather than silently skipping.
- `scan_or_block` gains a **shallow** scan over copied file names/extensions so
  an obviously-hostile payload can block the import. The *trust decision* is
  unchanged: the bundle inherits the skill's install trust / signature, exactly
  as the skill body already does.
- The same copy applies to the `commands/` install path only if a command
  skill ships siblings; in practice only `skills/<dir>/` bundles carry assets,
  so the first cut wires the `skills/` loop and leaves `commands/` as-is.

### 3. Injection — one resolved line

At the layer-3 injection call site (`mur-agent-runtime` task_runner, where
`mur_home`, agent name, and skill name are all in scope), the skill directory
is `agent_skills_dir(mur_home, agent).join(name)` — already computable, no new
field threaded through `layer3_body`.

After building the layer-3 body: if the skill directory contains **anything
besides `skill.yaml`**, append:

```
Bundled files for this skill are on disk at: <abs skill dir>
(e.g. run a script with the bash tool: `bash <abs skill dir>/scripts/foo.sh`)
```

If the directory holds only `skill.yaml`, append nothing (byte-identical to
today's behavior for asset-free skills).

That is the entire runtime change. The agent's existing bash tool (`cwd`) and
file tools do the rest.

### 4. Security

- **Reuse, don't invent.** Script execution is already gated by the agent's
  bash-tool entitlements and the HITL risk gate. Imported scripts are
  third-party code and inherit the skill's install trust (TOFU / signature via
  the existing `skill_bundle` / `skill_verify` path) — the same trust basis the
  injected skill *body* already relies on.
- **Path safety at import** (§2) is the one genuinely new check: a bundle
  cannot write outside its own install directory.
- **No auto-run.** Nothing executes at import or at injection time. A script
  runs only if the agent's own reasoning plus the bash-tool gate allow it.

### 5. Testing

Minimal, targeted:

- **Import — preserve:** a source skill with `scripts/foo.sh` imports so that
  `foo.sh` lands in the installed skill dir.
- **Import — reject escape:** a bundle entry `../escape` (or an out-of-dir
  symlink) fails the import.
- **Injection — present:** a skill dir containing a bundled file yields a
  layer-3 body that includes the on-disk path line.
- **Injection — absent:** a skill dir with only `skill.yaml` yields no path
  line.

## Affected code

| Layer | File | Change |
|-------|------|--------|
| Import | `mur-core/src/cmd/agent/addon/import.rs` | copy non-`SKILL.md` siblings into dest; path-safety check |
| Import scan | `addon/import.rs` (`scan_or_block` call path) | shallow scan of copied file names |
| Injection | `mur-agent-runtime/src/skills/` (layer-3 call site) | append resolved skill-dir path line when bundle present |

## Limitation restated

Parity is complete for agents co-located with a human (the concierge `mur`).
For headless agents, bundled scripts execute but any browser-facing half is
inert. Browser-forwarding is a separate, larger effort and is out of scope.
