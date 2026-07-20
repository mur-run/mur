# GitHub Directory Skill Install — Design

**Date:** 2026-07-20
**Status:** Design (approved for spec)

## Problem

MUR's "Install from URL" (Hub GUI Skills tab, and `mur-core/src/cmd/agent/skill_remote.rs`)
does a single HTTP GET and expects the body to be one skill manifest (YAML or markdown).
Pasting a GitHub directory URL such as
`https://github.com/obra/superpowers/tree/main/skills/brainstorming` fails: the GET returns
GitHub's HTML page, not the skill files, and the parser rejects it.

This blocks installing **multi-file skills** — a skill directory that contains a manifest
plus sibling files (e.g. `visual-companion.md`) and bundled `scripts/` (e.g.
`scripts/start-server.sh`). The only current multi-file path is an archive URL
(`.zip`/`.tar.gz`), which most upstream skills are not published as.

## Goal

Let "Install from URL" accept a GitHub directory URL pointing at a skill directory,
fetch the whole directory (manifest + siblings + `scripts/`), scan the bundled scripts for
suspicious content, surface findings in the existing consent preview, and install on accept.

Non-goal: executing any bundled script. MUR never runs them; import is copy-only.

## Reuse map

B is ~90% existing machinery. The addon/plugin import path (`mur agent addon import`) already
clones a repo and imports multi-file skills with their assets. B wires that capability to the
URL box and adds a subdirectory selector plus a script scanner.

| Concern | Mechanism | Status |
|---|---|---|
| Clone the repo | `skill_registry::git_clone_or_pull` (shallow, cached in addon cache) | existing |
| `SKILL.md` → MUR `SkillManifest` | `addon::parse::skill_md_to_manifest` | existing (C) |
| Structural safety (symlink / `..` / unsafe names) | `addon::import::validate_bundle` | existing |
| Copy skill dir including `scripts/` | `addon::import::copy_bundle_inner` | existing (#659/#660) |
| Manifest security scan | `mur_common::skill::scan::scan_skill` | existing |
| Preview + consent + `acceptFindings` | `agent_skill_preview_url` / `agent_skill_install_url` | existing (extend findings) |
| **GitHub dir URL parse** | `parse_github_dir(url) -> (clone_url, git_ref, subdir)` | **new (small)** |
| **Script content scan** | `scan_scripts(dir) -> Vec<Finding>` | **new (small)** |

## Data flow

```
URL box → classify → clone → select subdir → manifest + script scan → consent preview → install
```

Routing extends the existing `install_any_url` / `preview_any_url` fork in `skill_remote.rs`:

```
is_archive_url(url)      → bundle path            (existing)
parse_github_dir(url)    → clone + import path     (new: reuses addon import)
otherwise                → single HTTP GET         (existing)
```

## Components

### `parse_github_dir(url) -> Option<(clone_url, git_ref, subdir)>`

Recognizes GitHub hosts only. Forms:

- `github.com/<owner>/<repo>/tree/<ref>/<path...>` → `(https://github.com/<owner>/<repo>.git, <ref>, <path>)`
- `github.com/<owner>/<repo>` (bare) → `(…, default branch, "")` — subdir is the repo's skills root

Returns `None` for any non-GitHub host so the caller falls through to the single-file GET.

### clone + subdir selection

Reuse `git_clone_or_pull` (shallow `--depth=1`) into the addon cache. After clone, resolve the
subdir. Two shapes are accepted:

1. Subdir **is** a skill dir (contains `SKILL.md`) → single skill.
2. Subdir contains a `skills/` tree (repo/plugin root) → all discovered skills (reuse the
   addon `skills/<dir>/SKILL.md` walk).

Every candidate goes through the existing pre-write gates: `safe_member_name`, `validate_bundle`,
`scan_or_block`, dest-exists check. "All checks pass before a single byte is written" is preserved.

### `scan_scripts(dir) -> Vec<Finding>` (the security decision — scan, flag, don't block)

- **Scope:** all text files under the skill dir — `.sh .py .js .rb .pl .ps1`, plus extensionless
  files whose first bytes are a shebang. Binary or oversized files are skipped and flagged with a
  "binary/unscanned attachment" note (an unreadable attachment warrants a warning, not silence).
- **Patterns (conservative v1):** `curl|wget … | sh`, `rm -rf`, reverse shell
  (`bash -i >& /dev/tcp`), `eval`, base64-decode-then-exec, writes to `~/.ssh`, persistence via
  `launchctl` / `crontab`. Each hit records a finding with file name + line.
- **Non-blocking:** findings are merged into `SkillPreview.findings` alongside the manifest scan.
  The user sees them in the consent screen and accepts via the existing `acceptFindings` flag.
- **Never executes.** The spec states plainly that "scanned" ≠ "safe": MUR only copies; whether a
  script runs later is the agent's decision under its own entitlements / HITL gate.

## Tradeoff (accepted)

To fetch one subdirectory, git must shallow-clone the whole repo — git has no cheap
arbitrary-subdir clone. `obra/superpowers` is small; a GB-scale monorepo tree URL would pull the
whole repo. Mitigation: a post-clone size ceiling (default 50 MB, configurable) that aborts before
import. The GitHub `contents/` API alternative (fetch only the subtree, no clone) is rejected — it
needs a brand-new recursive fetcher and hits API rate limits, violating the reuse-first principle.

## Error handling

- Non-GitHub host tree URL → "only GitHub directories are supported; paste a raw file or a
  .tar.gz bundle".
- Clone failure, subdir missing, subdir has no `SKILL.md` → specific messages naming the actual
  cause, not a generic failure.
- Clone exceeds the size ceiling → abort with the measured size and the ceiling.

## Testing

- `parse_github_dir`: table test over tree URLs, bare repo URLs, non-GitHub URLs, malformed paths.
- `scan_scripts`: fixture skill dir with a malicious `.sh` → finding is emitted AND install is
  **not** blocked (accept path still reachable).
- End-to-end: install from a localhost git repo (reuse the existing addon-import test harness).

## Surface

- Primary: Hub GUI "Install from URL" box (routes through the extended `skill_remote.rs`).
- Because the machinery is CLI-callable, the same routing is reachable from
  `mur skill install <github-dir-url>` at no extra cost; wire it if trivial, otherwise defer.
