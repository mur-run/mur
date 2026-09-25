# browser skill: three triggers collapse into one hub + reference bundle

**Status:** Approved in conversation 2026-09-25. Not yet implemented.
**Scope:** `mur-browser/` (new, source of truth) · `mur-core/src/cmd/sync_cmd.rs`
(`ensure_mur_skill`, bundle-write step) · `mur-core/src/cmd/agent/cli/app/slash.rs`
(new `SlashCmd::Browser`) · `mur-core/src/cmd/agent/cli/slash_cmds.rs` (dispatch arm)
· `mur-core/src/cmd/agent/cli/turn.rs` (`submit()`'s mode-argument path — see D5).
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
| D2 | **`mur-browser/SKILL.md` is authored as markdown with Anthropic frontmatter, but `ensure_mur_skill` converts it to canonical YAML before writing `skill.yaml`.** Every other entry in that function's table is already canonical YAML fed straight to `std::fs::write(dir.join("skill.yaml"), content)` (`sync_cmd.rs:1422`) — none of them round-trip through a parser first, because none of them need to. `mur-browser/SKILL.md` does: `mur_common::skill::loader` reads `skill.yaml` only through `parse_canonical` (`mur-common/src/skill/loader.rs:115`), which rejects Anthropic frontmatter outright, so writing the raw markdown there ships a manifest the loader cannot load. The fix is one call at the write site, not a format change: `let manifest = mur_common::skill::parse_markdown(include_str!(...))?; let yaml = mur_common::skill::serialize_canonical(&manifest)?;` — same `parse_markdown` the non-`.yaml` branch of `cmd_skill_add` already uses (`mur-core/src/cmd/agent/skill.rs:127`) — then `yaml` (not the raw markdown) goes to `skill.yaml`, and the existing `yaml_to_markdown(content)` call for the rendered `SKILL.md` (`sync_cmd.rs:1424-1425`) takes that same canonical `yaml`, not the original markdown, so the two files stay in sync the same way every other entry's do. | Hand-write canonical `skill.yaml` directly — more ceremony for a hub whose whole body is "read the argument, load the matching reference file," and markdown is the easier authoring surface; the conversion happens once at build/install time, so authoring in markdown costs nothing once D2 is implemented correctly. |
| D3 | **`ensure_mur_skill` gains a second, parallel write step for bundle assets** (`sync_cmd.rs:1202`, right after the existing manifest loop). Today that function writes only `skill.yaml` + rendered `SKILL.md` per entry (`sync_cmd.rs:1420-1426`) — no builtin skill has ever shipped a `references/` dir centrally; that capability exists today only in the per-agent `cmd_skill_add` path (`mur-core/src/cmd/agent/skill.rs:152-172`), which copies from disk next to the source file. The new step is a small `(skill_name, relative_path, content)` list of `include_str!`'d reference docs, written to `mur_skills_dir.join("browser").join("references").join(<file>)`, run once directly after the manifest loop and before `symlink_skill_dir` — so the existing whole-directory symlink into `.claude`/`.augment`/`.agents` carries `references/` along for free, no change to that step. | Have `ensure_mur_skill` shell out to `cmd_skill_add`'s bundle-copy logic — that function reads assets from a filesystem path (`src.parent()`), but `ensure_mur_skill`'s content is compile-time `include_str!`, not a file on disk at install time; the two paths have different sources and don't unify without inventing a temp-file bridge for no benefit. |
| D4 | **`browser` is opt-in per agent**, matching how `browser-auth`/`browser-test`/`browser-automation` already worked — not auto-attached to every new agent's profile. | Auto-attach on `mur agent create` — changes default behavior for every new agent everywhere, a bigger blast radius than this change's actual ask. |
| D5 | **`/browser` is a real `SlashCmd::Browser(Vec<String>)` variant end to end — including the mode-argument case — not a partial fallthrough into `matched_skill`.** `matched_skill` is only ever consulted from one call site, inside `submit()`'s `matches!(cmd, SlashCmd::Unknown(_))` guard (`turn.rs:20`): `parse_slash` has to return `SlashCmd::Unknown("browser")` for that branch to run at all. A dedicated `SlashCmd::Browser(Vec<String>)` variant is never `Unknown` — the parser matches `"browser"` directly and returns the typed variant — so it never reaches that guard; it goes straight to `handle_slash`, whose arm returns, and `submit()` exits without ever calling `start_turn`. The corrected design: the dispatch arm for `SlashCmd::Browser(args)` handles all three cases itself — `--add` attaches the skill (D6); not-yet-attached prints the same "not installed, try /browser --add" message `/skill` already uses; **attached with a mode argument (or none) calls `turn::start_turn(app, format!("Use the browser skill. {}", args.join(" ")), tx)` directly** — the same string shape `matched_skill` would have produced, but constructed and sent explicitly, since there's no `Unknown` variant left for the generic fallthrough to catch. `turn::start_turn` is already `pub(super) fn` in the same module tree as `slash_cmds.rs` (`turn.rs:133`), so no visibility change needed — just an explicit call instead of relying on the fallthrough. | Route everything through `matched_skill` — this is what the first pass of this spec proposed, and it silently drops every mode-argument invocation once `browser` is a typed enum variant instead of `Unknown`: `/browser testing` after attach would print nothing back to the model, just return. A bespoke built-in with a static hardcoded 3-line menu (rejected alternative from the first design pass) — drifts out of sync with the skill's own logic and preempts the hub's own "ask which mode" behavior with a second, competing menu. |
| D6 | **`/browser --add` reuses `cmd_skill_add` unmodified, via the same `manage::skill_add` wrapper `/skill add` already calls.** Once D3 ships `~/.mur/skills/browser/SKILL.md` + `~/.mur/skills/browser/references/*.md` globally, `cmd_skill_add(agent, "<mur_root>/skills/browser/SKILL.md")` already does everything needed: parses the markdown manifest (frontmatter-only round trip — see D2 for why the *global* copy in `~/.mur/skills/browser/skill.yaml` is canonical while `SKILL.md` there stays the markdown rendering `cmd_skill_add` can also parse via its `else` branch, `mur-core/src/cmd/agent/skill.rs:127`), validates + scans it, writes the agent-scoped `skills/browser/skill.yaml`, and — because `BUNDLE_ASSET_DIRS` copies `references/`/`scripts/`/`assets/` from `src.parent()` (`mur-core/src/cmd/agent/skill.rs:162-172`) — picks up the three reference docs from the same global install directory. No new copy logic anywhere; the dispatcher arm just calls `manage::skill_add(agent, <global browser SKILL.md path>)`, the same wrapper `/skill add` already uses, and prints the same `RESTART_HINT`. | Write a bespoke attach path that copies files directly — duplicates validation, scanning, and bundle-copying that `cmd_skill_add` already does correctly; two code paths for one operation invites drift. |

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

1. New table entry — **not** the raw `include_str!` fed straight to `skill.yaml`
   like the canonical-YAML siblings. Immediately before the manifest-write loop
   (`sync_cmd.rs` around line 1407), convert once:
   ```rust
   let browser_manifest = mur_common::skill::parse_markdown(
       include_str!("../../../mur-browser/SKILL.md"),
   )?;
   let browser_yaml = mur_common::skill::serialize_canonical(&browser_manifest)?;
   ```
   then add `("browser", browser_yaml.as_str())` to the `skills` table so it
   flows through the existing loop unchanged: `skill.yaml` gets `browser_yaml`
   (parseable by `parse_canonical`, per D2), and the existing
   `yaml_to_markdown(content)` call (`sync_cmd.rs:1424-1425`) renders `SKILL.md`
   from that same canonical YAML, not from the original markdown source.
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
/// attaches the skill to this agent; a mode argument (once attached) is sent
/// to the model directly as a turn (see D5 — there is no `Unknown` fallthrough
/// once this is a typed variant).
Browser(Vec<String>),
```
parser arm: `"browser" => SlashCmd::Browser(words.map(str::to_string).collect())`.

`slash_cmds.rs` dispatch arm — all three cases handled here, none deferred to
`turn.rs`'s `matched_skill` guard (which this variant never reaches):
```rust
SlashCmd::Browser(args) => {
    if args.first().map(String::as_str) == Some("--add") {
        run_manage(app, move |agent| manage::skill_add(agent, &global_browser_skill_md_path)).await
    } else if !app.skills.iter().any(|s| s.name == "browser") {
        app.push_system("browser skill not attached — run /browser --add first");
    } else {
        let instruction = if args.is_empty() {
            "Use the browser skill.".to_string()
        } else {
            format!("Use the browser skill. {}", args.join(" "))
        };
        app.clear_input();
        turn::start_turn(app, instruction, tx);
    }
}
```
`turn::start_turn` is `pub(super) fn` in `turn.rs:133`, already visible from
`slash_cmds.rs` under the same `cli` module tree — no visibility change.
`app.skills` naming above is illustrative; use whatever field
`complete::load_agent_skills`/`MenuContext` actually exposes for "is this skill
attached" (confirm exact field name during implementation, not guessed here).

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

## 5. Verification checklist (for implementation, not yet run)

- `parse_canonical` succeeds on the written `~/.mur/skills/browser/skill.yaml`
  (i.e. `mur skill validate browser` or equivalent loader smoke test) — the
  concrete regression D2 exists to prevent.
- `/browser --add` on an agent with no `browser` skill attaches it and prints
  `RESTART_HINT`; `mur agent card <agent>` (or profile read) shows
  `skills/browser` afterward.
- `/browser testing` *after* attach reaches the model as a turn — observed via
  the channel/transcript, not just "the command returned" (mur-verification:
  agent-reported completion is not evidence).
- `/browser testing` *before* attach prints the not-attached message and does
  **not** start a turn.
- `references/{auth,testing,automation}.md` exist under both
  `~/.mur/skills/browser/references/` and the agent-scoped
  `skills/browser/references/` after `--add` (bundle copy, D3 then D6).

## 6. Revision note

The original pass of this spec (D2, D5) assumed `mur-browser/SKILL.md` could
be written to `skill.yaml` as-is, and that a `SlashCmd::Browser` variant would
still reach `matched_skill`'s fallthrough for mode arguments. Both were wrong
against the actual code (`sync_cmd.rs:1422` writes `content` verbatim with no
parse step; `turn.rs:20`'s `matched_skill` call is gated on
`SlashCmd::Unknown`, which a typed variant never is) and were caught in
review before implementation started — no code had been written yet. D2 and
D5 above are the corrected versions; this section exists so the reasoning for
the correction isn't lost.
