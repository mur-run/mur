# Built-in Role Skills + Registry One-Click Install — Design

**Date:** 2026-07-04
**Status:** Approved (brainstormed with David)

## Problem

1. Design conversations (brainstorming) happen outside the fleet — in the
   supervisor's head — so the default MUR concierge should ship with a
   brainstorming skill built in, for every Hub user, not just this machine.
2. The Dashboard (app.mur.run) lists official Skills / MCP Servers /
   Workflows, and the Hub has Plugins — four install surfaces with no
   unified "click on Dashboard → installed in my Hub" path.
3. Built-in skills baked into the app bundle would be frozen; they need the
   same auto-upgrade behavior plugins already have.
4. Fleet role agents (coder, pm, qa, repo) should come with methodology
   skills (superpowers derivatives) matched to their role.

## Design

### A. Built-in brainstorming skill (default concierge)

- Rewrite `superpowers:brainstorming` as a MUR skill (derived work, MIT
  attribution) — human-in-the-loop dialogue: one question at a time,
  2–3 approaches with a recommendation, sectioned design presentation,
  ends by handing the agreed design to `pm` for spec/plan authoring.
- Placement rationale: in a fleet only the concierge (`mur`) has a human
  conversation surface; dialogue skills on members degrade into
  self-talk and router relays compress >2–3KB content (proven gotcha).
  `pm` keeps its meaning as the artifact/tracking role — it is NOT the
  brainstormer. BMAD-method is not adopted (it duplicates what MUR fleet
  already is); only its phase structure is borrowed:
  brainstorm (mur↔human) → spec (pm) → implement (coders) → qa → repo.
- Ship in the bundled concierge template (`mur-hub-gui` `seed_mur.rs`
  template dir): `skills/brainstorming/skill.yaml` + profile reference +
  regression test mirroring the existing concierge-skill test.
- Template snapshot is an **offline first-run seed only**. It seeds a
  skill only when absent; it never overwrites anything. Upgrades flow
  through the registry channel (C).

### B. Dashboard one-click install (web → Hub bridge)

**Deep link** (the vscode:// pattern), via the Tauri deep-link plugin:

```
mur://install?type=skill|mcp|workflow|plugin&url=<https source>&name=<display>
```

- Dashboard cards get an "Install to Hub" button = a `mur://` anchor,
  plus a "copy CLI command" fallback (`mur agent skill install <url>`,
  `mur agent mcp add-remote …`) for users without the Hub.
  Dashboard side is UI-only; no server backend needed.
- Rejected alternatives: localhost HTTP from dashboard JS (CORS, port
  probing by arbitrary pages — worst security); relay-based install
  queue (cross-device, needs account/queue/poll — deferred to P2).

**Security (fail-closed, non-negotiable):** any web page can forge a
`mur://` link, therefore:

- The Hub NEVER auto-installs from a deep link. Every install routes
  through the existing fetch → preview → consent modal (source URL,
  content summary, security-scan result) before anything is written.
- `url` must be https (reuse `validate_skill_url`; http only for
  localhost dev). Official registry domains show an "official" badge;
  any other domain shows a prominent warning.
- Cold start: if the Hub isn't running the OS launches it; the
  deep-link plugin delivers the pending URL, consent modal opens then.

**Routing per type — all reuse existing installers:**

| type | pipeline |
|---|---|
| `skill` | quill remote-skill pipeline (`mur-core/src/cmd/agent/skill_remote.rs`): validate → fetch → parse+scan preview → consent → install to chosen agent or user scope |
| `mcp` | feather pipeline (`mcp add-remote` engine: probe, OAuth/bearer, keychain, consent) |
| `plugin` | Hub's existing plugin discover/import |
| `workflow` | the one new installer: fetch yaml → reuse quill's scan/consent shell → write `~/.mur/workflows/` |

### C. Upgrade pipeline for built-in / registry skills

Built-in ≠ a fork baked into the binary. Built-in = **pre-installed
official registry skill**. The registry copy is the source of truth;
the bundled template is an offline snapshot.

Skill manifests gain origin stamps:

```yaml
origin: registry:mur-official/brainstorming
origin_version: 1.0.0
origin_hash: sha256:…        # content hash as shipped
```

The Hub's existing plugin update check extends to every skill with an
`origin: registry:*` stamp:

- registry version > local `origin_version` AND local content hash ==
  `origin_hash` (user has not modified) → auto-upgrade, restamp.
- user-modified (hash mismatch) → never overwrite; notify "official
  update available, you have local changes" with keep / overwrite /
  diff choices. Reuses quill's drift detection.
- Hub app upgrades re-run the seeder: seed only missing skills; the
  template never overwrites.

Upstream (github.com/obra/superpowers) updates do NOT auto-sync: our
skills are rewritten derivatives; the publisher reviews upstream
changes and bumps the registry version manually.

One mechanism covers built-in skills, Dashboard-installed skills, and
plugins — no second upgrade system.

### D. Role skill packs (superpowers derivatives for fleet roles)

All published in the official registry as rewritten MUR skills (same
origin/upgrade pipeline as C). Superpowers skills target the Claude
Code harness (subagent dispatch, plan mode, Skill tool); each must be
**ported**, mapping "dispatch subagent" onto fleet delegation or plain
sequential execution — not copy-pasted.

| Role | Skills | Notes |
|---|---|---|
| concierge (`mur`) | brainstorming | dialogue surface |
| pm | writing-plans | brainstorming's terminal state; joins pm's existing spec/PRD skills |
| coder (rustsmith, frontend) | executing-plans, test-driven-development, using-git-worktrees, systematic-debugging, verification-before-completion, receiving-code-review | last three added in review: debug methodology, "verify before claiming done" (counters false completion reports), rigor when receiving review feedback |
| qa | code-review (ported from requesting-code-review, **semantics inverted**: in superpowers the implementer requests a review; in a fleet qa IS the reviewer, so qa's skill is "how to perform a review") | |
| repomanager | finishing-a-development-branch | merge/PR decisions belong to the repo role per fleet division of labor (David initially suggested rustsmith; changeable at review) |

Registry skills carry `recommended_roles: [concierge|pm|coder|qa|repo]`.
The Hub agent-creation wizard and Dashboard filter by role so a new
agent gets its role pack in one action instead of per-skill installs.

## Out of scope (deferred)

- Relay-based cross-device install queue (P2)
- Installed-state feedback on the Dashboard (needs a return channel, P2)
- Signature verification beyond quill's existing TOFU
- Auto-tracking upstream superpowers releases

## Implementation order

1. **C first** (origin stamps + update check) — A and D depend on it
2. **A** (port brainstorming, seed template, registry publish)
3. **D** (port the remaining role skills, `recommended_roles`, wizard pack UI)
4. **B** (deep link plugin + consent routing + workflow installer + Dashboard buttons)
