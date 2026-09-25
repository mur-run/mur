# browser skill: three triggers collapse into one hub + reference bundle

**Status:** Approved in conversation 2026-09-25. Not yet implemented.
**Scope:** `mur-browser/` (new, source of truth) · `mur-core/src/cmd/sync_cmd.rs`
(`ensure_mur_skill`, bundle-write step) · `mur-core/src/cmd/agent/cli/app/slash.rs`
(new `SlashCmd::Browser`) · `mur-core/src/cmd/agent/cli/slash_cmds.rs` (dispatch arm).
No protocol or runtime change; no other crate touched.

## 1. Problem

Three skills — `browser-auth`, `browser-test`, `browser-automation` — exist
only in this session's `~/.mur/skills/`, installed by hand via
`mur skill install /tmp/.../SKILL.md`. That path never touches
`ensure_mur_skill`, the one function that ships a skill to every MUR install
(`mur-core/src/cmd/sync_cmd.rs:1202`, a `&[(&str, &str)]` table of
`include_str!`'d manifests written on every `mur init`/`mur sync`). Nobody
else who installs MUR has ever gotten these three skills — a direct hit on
CLAUDE.md's Mandatory Rule 1 ("design skills for every MUR install, not just
this machine").

Separately, three independently-triggered skills for one domain (browser
work) is more surface than the domain needs: the user has to know which of
three names to reach for. The user asked for one hub, `mur-browser`, that
picks a mode from an argument, backed by reference docs it reads rather than
three standalone skills.

## 2. Decisions

| # | Decision | Rejected alternative |
|---|---|---|
| D1 | **Source of truth is `mur-browser/` in the repo**, versioned like source: `mur-browser/SKILL.md` (hub) + `mur-browser/references/{auth,testing,automation}.md` (mode bodies, frontmatter stripped). `browser-test` renames to `testing.md` per the user's own wording. | Keep the three as separate `mur-core/src/skills/*.yaml` table entries — defeats the point of collapsing three triggers into one; a caller would still need to pick the right skill name up front. |
| D2 | **`mur-browser/SKILL.md` is markdown with Anthropic frontmatter**, parsed via the existing `mur_common::skill::parse_markdown` (same path `cmd_skill_add`'s non-`.yaml` branch already uses) — not hand-written canonical YAML. | Canonical `skill.yaml` — more ceremony for a hub whose whole body is "read the argument, load the matching reference file," and markdown is what installed cleanly this session. Flagged as a low-risk assumption per CLAUDE.md Rule 2; revisit if the user actually wanted YAML. |
| D3 | **`ensure_mur_skill` gains a second, parallel write step for bundle assets** (`sync_cmd.rs:1202`, right after the existing manifest loop). Today that function writes only `skill.yaml` + rendered `SKILL.md` per entry (`sync_cmd.rs:1420-1426`) — no builtin skill has ever shipped a `references/` dir centrally; that capability exists today only in the per-agent `cmd_skill_add` path (`mur-core/src/cmd/agent/skill.rs:152-172`), which copies from disk next to the source file. The new step is a small `(skill_name, relative_path, content)` list of `include_str!`'d reference docs, written to `mur_skills_dir.join("browser").join("references").join(<file>)`, run once directly after the manifest loop and before `symlink_skill_dir` — so the existing whole-directory symlink into `.claude`/`.augment`/`.agents` carries `references/` along for free, no change to that step. | Have `ensure_mur_skill` shell out to `cmd_skill_add`'s bundle-copy logic — that function reads assets from a filesystem path (`src.parent()`), but `ensure_mur_skill`'s content is compile-time `include_str!`, not a file on disk at install time; the two paths have different sources and don't unify without inventing a temp-file bridge for no benefit. |
| D4 | **`browser` is opt-in per agent**, matching how `browser-auth`/`browser-test`/`browser-automation` already worked — not auto-attached to every new agent's profile. | Auto-attach on `mur agent create` — changes default behavior for every new agent everywhere, a bigger blast radius than this change's actual ask. |
| D5 | **`/browser` is a real `SlashCmd::Browser(Vec<String>)` variant**, not routed through the generic `matched_skill` fallthrough that `/brainstorming`-style commands use. That fallthrough only fires once a skill is already in `app.skills` (`mur-core/src/cmd/agent/cli/app/slash.rs` parse path + `turn.rs`'s `matched_skill` check) — since D4 keeps `browser` opt-in, the *first* thing a user needs before the skill exists locally is a way to attach it, and that has to work before `matched_skill` has anything to match. `/browser --add` becomes the attach path, mirroring `/skill add`'s own `Vec<String>` shape (`app/slash.rs`'s existing `SlashCmd::Skill(Vec<String>)`). Once attached, bare `/browser <mode>` (or no mode) rides the normal `matched_skill` fallthrough like any other skill — no separate dispatch needed for that part. | Route everything through `matched_skill` — breaks for a user who has never attached the skill; `/browser --add` would have nowhere to live. A bespoke built-in with a static hardcoded 3-line menu (rejected alternative from the first design pass) — drifts out of sync with the skill's own logic and preempts the hub's own "ask which mode" behavior with a second, competing menu. |
| D6 | **`/browser --add` reuses `cmd_skill_add` unmodified.** Once D3 ships `~/.mur/skills/browser/SKILL.md` + `~/.mur/skills/browser/references/*.md` globally, `cmd_skill_add(agent, "<mur_root>/skills/browser/SKILL.md")` already does everything needed: parses the markdown manifest, validates + scans it, writes the agent-scoped `skills/browser/skill.yaml`, and — because `BUNDLE_ASSET_DIRS` copies `references/`/`scripts/`/`assets/` from `src.parent()` (`mur-core/src/cmd/agent/skill.rs:162-172`) — picks up the three reference docs from the same global install directory. No new copy logic anywhere; the dispatcher arm just calls `manage::skill_add(agent, <global browser SKILL.md path>)`, the same wrapper `/skill add` already uses, and prints the same `RESTART_HINT`. | Write a bespoke attach path that copies files directly — duplicates validation, scanning, and bundle-copying that `cmd_skill_add` already does correctly; two code paths for one operation invites drift. |

## 3. Design

### 3.1 `mur-browser/` (new directory, repo root)

```
mur-browser/
  SKILL.md                  — hub: frontmatter (name: browser, description),
                               body reads the invocation argument
  references/
    auth.md                 — from browser-auth's SKILL.md body, frontmatter stripped
    testing.md               — from browser-test's SKILL.md body (renamed)
    automation.md            — from browser-automation's SKILL.md body
```

`SKILL.md`'s body: if the caller supplied an argument matching `auth`,
`testing`, or `automation`, load and follow the matching reference file. If no
argument (or an unrecognized one), ask the user which of the three they want
— one question, mur-grilling style, with a recommendation attached — rather
than guessing.

### 3.2 `ensure_mur_skill` — two additions

1. New table entry, same shape as every sibling:
   ```rust
   ("browser", include_str!("../../../mur-browser/SKILL.md")),
   ```
2. Immediately after the existing manifest-write loop (`sync_cmd.rs` around
   line 1426), a second loop specific to skills with a bundle — starting with
   just `browser` — writing each `include_str!`'d reference file to
   `mur_skills_dir.join(<skill_name>).join("references").join(<file_name>)`.
   Runs before `symlink_skill_dir`, so the per-tool-dir symlink (which
   symlinks the whole skill directory, not individual files) carries
   `references/` along unchanged.

### 3.3 Slash command

`app/slash.rs`:
```rust
/// `/browser [--add|auth|testing|automation]` — browser skill hub. `--add`
/// attaches the skill to this agent; a mode argument (once attached) is
/// forwarded to the mur-browser skill via the normal matched_skill path.
Browser(Vec<String>),
```
parser arm: `"browser" => SlashCmd::Browser(words.map(str::to_string).collect())`.

`slash_cmds.rs` dispatch arm:
- `args.first() == Some("--add")` → `run_manage(app, move |agent| manage::skill_add(agent, &global_browser_skill_md_path))`, same call shape as the existing `SlashCmd::Skill(args)` arm.
- otherwise, if `browser` is not yet in the agent's attached skills → push a short system message pointing at `/browser --add` (mirrors `/skill`'s own "not installed" messaging pattern already in the codebase — no new string family invented).
- otherwise → fall through to the same `matched_skill` path other attached skills already use, forwarding the raw args as the turn's instruction (e.g. `"Use the browser skill. testing"`).

### 3.4 Cleanup, same change

- Delete the three ad-hoc `~/.mur/skills/browser-{auth,test,automation}`
  directories installed this session — the hub supersedes them, they are not
  kept in parallel.
- Any plan-doc checkbox history created for those three ad-hoc installs this
  session is superseded, not preserved.

## 4. Out of scope

- Auto-attaching `browser` to new agents (D4) — explicitly opt-in per the
  user's call.
- Splitting `sync_cmd.rs` (already at 2412 lines, pre-existing debt) — this
  change adds one table entry and one small loop, not enough to justify a
  split on its own; flagged as a candidate for separate follow-up if it grows
  past 800 lines.
