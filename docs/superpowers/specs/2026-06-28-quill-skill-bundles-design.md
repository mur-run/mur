# Quill skill bundles — install a skill pack (.zip / .tar.gz) by URL

**Status:** Design / spec
**Date:** 2026-06-28
**Builds on:** quill P1 (install a single skill by URL — `mur-core/src/cmd/agent/skill_remote.rs`, the Hub Install-from-URL modal). This adds **archive** support.

---

## 1. Problem

quill P1 installs a single raw skill (`skill.yaml`/`.md`) from a URL. Users also have skills packaged as archives — a single zipped skill or a **pack of several skills**. P1 falls through to the YAML parser on a `.zip`/`.tar.gz` and fails with a confusing "not a valid skill manifest." This adds first-class archive support.

MUR skills are **single-file** (`skill.yaml`/`.md`; the model has no asset attachments), so a bundle is meaningfully **one-or-more skills in an archive**, not a single skill plus loose files.

## 2. Decisions (from brainstorming)

- **Formats:** `.zip` AND `.tar.gz`/`.tgz` (both dependency sets already present — see §4).
- **Contents:** an archive holds **one or more** skill manifests (`skill.yaml`/`.md`, any subdirectory depth). quill discovers all, scans + validates each, and installs the **clean** ones; skills with blocking findings install only on explicit acceptance (fail-closed).

## 3. What MUR already has (reuse map)

| Capability | Existing primitive | Location |
|---|---|---|
| Single-skill fetch/preview/install (https, size-cap, scan, fail-closed) | quill P1 `skill_remote::{validate_skill_url, fetch_*, preview_skill_text, install_skill_from_url}` | `mur-core/src/cmd/agent/skill_remote.rs` |
| Skill scan (executable + injection) | `scan_skill` / `ContentScanReport` | `mur-common/src/skill/scan/` |
| Parse `.yaml`/`.md` → manifest | `parse_canonical` / `parse_markdown` | `mur-common/src/skill/parser.rs` |
| Per-skill install (validate + scan + write + register) | `cmd_skill_add(agent, path)` | `mur-core/src/cmd/agent/skill.rs` |
| tar.gz **safe extraction** (zip-slip protection pattern) | `.muragent` `extract_payload`, `.fleet` import | `mur-common/src/muragent/installer.rs`, `mur-core/src/cmd/fleet/import.rs` |
| Archive deps | `tar`+`flate2` (mur-common & mur-core); `zip` v2 (mur-core) | Cargo.toml |
| Hub Install-from-URL modal | `SkillAddUrlModal` | `mur-hub-gui/ui/src/components` |

**Conclusion:** new code = archive detection + safe extraction + a multi-skill discover/preview/install layer over quill P1's per-skill primitives + a multi-skill consent UI. No new on-disk skill format.

## 4. Design

### 4.1 mur-core — `skill_bundle.rs` (new, sibling of `skill_remote.rs`)

- `fn is_archive_url(url) -> Option<ArchiveKind>` — `.zip` → `Zip`; `.tar.gz`/`.tgz` → `TarGz`; else `None`.
- `const BUNDLE_MAX_BYTES`, `const BUNDLE_MAX_ENTRIES`, `const BUNDLE_MAX_TOTAL_UNCOMPRESSED` — named caps (no literals).
- `fn extract_archive(bytes: &[u8], kind: ArchiveKind, dest: &Path) -> Result<()>` — **path-traversal-safe**: reject entries whose normalized path escapes `dest` (contains `..` or is absolute) or are symlinks; enforce entry-count + per-file + total-uncompressed caps (zip-bomb guard). For `TarGz` reuse the muragent extraction approach; for `Zip` mirror it with the `zip` crate.
- `fn discover_skills(dir: &Path) -> Vec<PathBuf>` — walk `dir`; return every file that parses as a skill (`skill.yaml`/`*.yaml` via `parse_canonical`, `*.md` via `parse_markdown`). Non-skill files are ignored (bundle may carry a README/LICENSE).
- `struct BundlePreview { skills: Vec<SkillPreview>, errors: Vec<String> }` (reuses quill P1's `SkillPreview { name, description, category, body, blocking, findings }`).
- `async fn preview_bundle_url(url) -> Result<BundlePreview>` — validate https → download (size-capped) → extract to a temp dir → discover → `preview_skill_text` each → return all previews (+ any per-file parse errors). Installs nothing.
- `async fn install_bundle_from_url(agent, url, accept_findings: bool) -> Result<Vec<String>>` — preview; if any skill is `blocking` and `!accept_findings`, **skip those** (do not install) and install the rest; with `accept_findings` install all. Each install reuses the per-skill path (`cmd_skill_add` on the extracted file). Returns the installed ids. Temp dir cleaned up always.

### 4.2 mur-core — route archive URLs

`install_skill_from_url` (P1) gains a front-door check: if `is_archive_url(url).is_some()`, delegate to `install_bundle_from_url`. So CLI `skill add-url` and the Hub both transparently handle archives. (A genuinely unsupported archive type — e.g. `.rar` — still gets the clear "not a valid skill manifest / unsupported" error; the confusing-`.zip` case from quill P1 §10a is now resolved by actually supporting it.)

### 4.3 Hub

- Tauri: `agent_skill_preview_url` returns either a single preview or a bundle preview — cleanest is a new `agent_skill_preview_bundle_url`/`agent_skill_install_bundle_url`, OR a unified `agent_skill_preview_any(url)` returning `{ kind: "single"|"bundle", skills: [SkillPreview] }`. **Chosen:** unify — preview returns a `Vec<SkillPreview>` (length 1 for a single skill), so the modal has one code path.
- `SkillAddUrlModal`: after Fetch, render the **list** of discovered skills, each with name/description + its findings; a single "install the N flagged skills anyway" checkbox gates the flagged ones; clean skills always install. Title/labels generalize from "skill" to "skill(s)".

### 4.4 CLI

`mur agent skill add-url <agent> <url> [--yes]` already routes through `install_skill_from_url`; with §4.2 it transparently installs archives too. Output lists each installed skill id.

## 5. Security model

- **Safe extraction is the new attack surface:** reject path-traversal (`..`/absolute/symlink entries), enforce entry-count + per-file + total-uncompressed caps (zip-bomb), https-only download, size-capped. Extraction happens in MUR's trusted control plane to a temp dir, never inside a sandboxed agent.
- Every discovered manifest is scanned (`scan_skill`); **fail-closed** per skill (flagged ones need explicit accept).
- Consent shows each skill's body + findings.
- Cryptographic bundle **signing** (à la `.muragent`) is out of scope here — deferred; trust rests on scan + consent.

## 6. Error handling

- Unsupported archive type / not an archive and not a skill → clear error.
- Corrupt archive, traversal/cap violation → abort, temp dir discarded, nothing installed.
- Zero skills found in the archive → clear "no skills found in bundle."
- Mixed valid/invalid entries → install the valid clean ones; report the invalid ones in `errors` (don't fail the whole bundle).
- Duplicate skill id (already installed) → existing `cmd_skill_add` behavior.

## 7. Testing

- Unit (network-free): `is_archive_url` extension detection; `extract_archive` rejects `../escape`, absolute paths, symlinks, and over-cap archives (build small in-memory zip + tar.gz fixtures); `discover_skills` finds nested skill.yaml/.md and ignores non-skills; `BundlePreview` aggregates findings; `install_bundle_from_url` skips flagged-without-accept and installs clean (temp-dir fixture).
- Gated `#[ignore]` network test against a real archive URL.
- Hub: modal renders a multi-skill list; flagged-gate works.
- Manual/live: install a 2-skill `.zip` (both clean → both install); a `.tar.gz` with one clean + one injection skill (clean installs, injection gated); a zip-slip archive (rejected).

## 8. Non-goals

- Cryptographic bundle signing/verification (later).
- Skill *assets* (MUR skills are single-file).
- Auto-update of installed bundles.
- A skill registry (that is quill P2).

## 9. Open questions

- Per-skill accept vs one "install all flagged" checkbox? (Lean: one checkbox — simpler; revisit if users want granular control.)
- Cap values for `BUNDLE_MAX_*` — pick conservative defaults (e.g. 5 MiB download, 256 entries, 32 MiB total uncompressed) and make them constants.
