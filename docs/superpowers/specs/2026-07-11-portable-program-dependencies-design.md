# Portable Program Dependencies — Design

**Status:** Design approved 2026-07-11. Phased implementation (Phase 1 first).

## §1 · Problem

A shared MUR artifact — an agent, a fleet, a skill, or an MCP server — often
needs an **external program** installed on the host to work: the deep-research
fleet needs `lightpanda`/`obscura` at `~/.mur/aura/`; an MCP server shells out
to `agent-browser` (npm), `gh`, `docker`, or `ffmpeg`; a skill's bundled script
needs `python`. When a recipient imports the bundle, those programs are **not**
in the bundle (they are machine-local binaries), MUR has **no mechanism to
declare or resolve them**, and the artifact silently breaks or degrades.

This is a **general portability problem**, not specific to deep-research.
Existing mechanisms do not cover it: `SkillManifest.requires` is skill→skill
(by name+version), `McpServerEntry.command` names a binary but not how to get
it, and there is no cross-artifact "what external programs does this need"
declaration or resolver.

## §2 · Scope & non-goals

**In scope:** a uniform way for any MUR artifact to (a) **declare** the external
programs it needs, (b) **detect** their presence cross-platform, (c) **report**
what is missing and why, and (d) **install** missing programs under a
trust-gated consent model.

**Non-goals:**
- Bundling the binaries themselves inside `.fleet`/`.muragent` (they stay
  machine-local; the bundle only *declares* them).
- A general package manager. This resolves a small set of declared programs,
  not arbitrary dependency graphs.
- Managing the *runtime* sandbox/egress of installed programs — that is the
  existing sandbox/egress subsystem's job. This design only gets the program
  onto disk.

## §3 · Core principle — detection and installation have different trust boundaries

- **Detection** (checking whether a program is present, and showing the user
  what is missing) reads only local state and displays declared metadata. It is
  **always safe** and runs automatically.
- **Installation** (downloading and placing an executable on the host) is
  security-sensitive: auto-running an installer a stranger declared is arbitrary
  code execution by design. A signature proves *who*, not that the program is
  *safe*. Therefore installation is gated by a **trust gradient** (§7): only
  MUR-curated recipes auto-install for everyone; author-declared programs
  auto-install only from a publisher the user has explicitly trusted; unknown
  sources are **detect-and-guide only**.

## §4 · Global constraints

- **No hardcoded values** — curated recipe URLs/checksums/paths live in a
  registry manifest, not inline literals; env/config overrides where a path is
  user-specific.
- **Cross-platform** — detection and installation MUST work on macOS
  (arm64/x86_64), Linux (arm64/x86_64), and Windows (x86_64). Recipes are keyed
  by `(os, arch)`.
- **Fail-safe / non-blocking** — a missing dependency NEVER hard-fails import or
  run; the artifact degrades (deep-research runs on tier-1 search/fetch without
  a render engine). Detection warns; it does not abort.
- **Consent-gated installs** — no program is downloaded/installed without an
  explicit user action (`install-deps`) or a per-item consent prompt; `--yes`
  only for scripted use the user opted into.
- **Integrity** — every curated download is verified against a **pinned
  SHA-256** before it is placed on disk or marked executable.
- **Reuse existing primitives** — extend `mur agent doctor`; reuse
  `publisher_trust.rs` + Ed25519 signing (skill security) for Phase 2; reuse
  `McpServerEntry.command`/`binary_sha256` for MCP-command detection.
- Files ≤ 800 lines; brand "MUR" in user-facing strings; comments/spec English.

## §5 · Declaration schema — `requires_programs`

A new optional list, declarable in `skill.yaml`, an `McpServerEntry`, an
agent's `profile.yaml` (agent-level deps not tied to a specific skill/MCP),
and `fleet.yaml`. Each entry:

```yaml
requires_programs:
  - name: lightpanda                       # stable identifier (lowercase slug)
    detect: { file: "~/.mur/aura/lightpanda" }   # see §6 — exactly one detect method
    reason: "deep-research render tier (JS pages)"  # human "why", shown in the report
    hint: "https://lightpanda.io/download"          # manual-install guidance (display only, always safe)
    registry: lightpanda                            # OPTIONAL: key into MUR's curated registry (§7.1) → enables auto-install
```

Rules:
- `name`, `detect`, and `reason` are required; `hint` and `registry` optional.
- The bundle **never** embeds an executable installer or a download URL that MUR
  auto-runs (Phase 1). Auto-install comes only from a curated `registry` match
  (Phase 1) or a signed publisher recipe (Phase 2, §7.2).
- **MCP command auto-detection (no declaration needed):** an `McpServerEntry`'s
  `command` is itself an external-program requirement. `doctor` synthesizes a
  `ProgramDep { name: <command>, detect: command:<command>, reason: "MCP server
  <mcp-name>" }` for every mounted MCP whose command is not an absolute path
  already present. Explicit `requires_programs` covers *helper* programs the
  MCP/skill spawns (e.g. the gateway spawning `lightpanda`).

## §6 · Detection (`detect`) — cross-platform, side-effect-free

Exactly one of:

| method | check |
|--------|-------|
| `file: <path>` | tilde/`$MUR_HOME`-expanded path exists (used for `~/.mur/aura/…`) |
| `command: <name>` | resolves on `PATH` — searches `PATH` dirs + MUR's `standard_exec_dirs`; appends `.exe`/`.cmd` on Windows |
| `version: { command: <argv>, min: <semver> }` | runs the command, parses the first semver in stdout, compares `>= min` |

Detection is pure (no downloads, no installs). `version` runs a bounded
subprocess only to read a version string; a nonzero exit or unparseable output
is treated as "present but unknown version" (reported, never auto-actioned).

## §7 · Trust gradient

### §7.1 Curated registry (Phase 1) — MUR-owned, auto-installs for everyone

MUR ships a built-in registry mapping a `registry` key to a per-platform recipe.
Format (shipped manifest, not inline literals):

```yaml
lightpanda:
  description: "Lightpanda headless browser (native render tier)"
  platforms:
    aarch64-macos: { url: "https://…/lightpanda-aarch64-macos", sha256: "…", install_to: "aura/lightpanda", executable: true }
    x86_64-macos:  { url: "…", sha256: "…", install_to: "aura/lightpanda", executable: true }
    aarch64-linux: { … }
    x86_64-linux:  { … }
    x86_64-windows:{ url: "…", sha256: "…", install_to: "aura/lightpanda.exe", executable: true }
```

- `install_to` is relative to `mur_home` (`~/.mur`); `url`+`sha256` are
  **MUR-maintained and MUR-vetted**. `install-deps` selects the current
  `(os,arch)` entry, downloads, verifies the pinned SHA-256, writes atomically
  (temp + rename), and sets the executable bit.
- Multi-file/tarball recipes: a recipe may declare an archive + an
  `extract`/`members` list (deep-research's obscura ships two binaries). The
  installer extracts, verifies each member, installs each `install_to`.
- Curated because the URL and checksum are owned by MUR, not the bundle author —
  so a malicious bundle cannot point "lightpanda" at malware; the worst it can
  do is *reference* a curated key, which resolves to MUR's vetted artifact.
- **Phase 1 seed entries:** `lightpanda`, `obscura` (+ `obscura-worker`).
  `agent-browser` is npm-installed → its recipe is a documented manual `hint`
  (npm global installs are out of scope for the download-and-verify installer).

### §7.2 Trusted-publisher recipes (Phase 2) — author-declared, trust-gated

A bundle from a **signed publisher** may carry a `program_recipe` (per-platform
url+sha256+install_to), signed by the publisher's Ed25519 key. Auto-install of
such a recipe is offered **only if** the user has elevated that publisher to a
high trust tier in the existing `publisher_trust` keyring (TOFU + explicit
elevation). Otherwise it degrades to §7.3. This reuses the skill-security trust
machinery; the recipe signature provides integrity + attribution, and the
user's trust-tier decision provides authorization. Per-install consent shows the
publisher, the URL, the checksum, and the install target before anything runs.

**Status:** Implemented (Phase 2). See implementation spec `2026-07-11-portable-program-dependencies-phase2-design.md`.

### §7.3 Unknown / untrusted — detect and guide only

Any declared program that is neither a curated key nor a trusted-publisher recipe
is **displayed** (name + reason + `hint`) and **never auto-installed**. The user
installs it themselves from the hint. This is the safe default and the fallback
for every tier when trust is absent.

## §8 · Command surface

Extend the existing `doctor`; add `install-deps`.

- **`mur agent doctor <name>`** / **`mur fleet doctor <name>`** — aggregate all
  declared `requires_programs` (from the agent's skills + mounted MCP servers,
  or the fleet's members) plus synthesized MCP-command deps (§5), run detection
  (§6), and print a report:

  ```
  Fleet 'deep-research' — external program dependencies:
    ✓ (search/fetch work with no external program)
    ✗ lightpanda   render tier (JS pages)         [curated]
        auto:   mur fleet install-deps deep-research
        manual: https://lightpanda.io/download
  1 missing — the fleet runs without it (render tier degrades to tier-1 text).
  ```

  `doctor` is read-only and always safe. It marks each missing dep's tier
  (`curated` / `publisher:<name>` / `manual`).

- **`mur agent install-deps <name>`** / **`mur fleet install-deps <name>`**
  `[--program <name>]` `[--yes]` — the consent-gated installer. For each missing
  dep that is curated (Phase 1) or a trusted-publisher recipe (Phase 2):
  download → verify SHA-256 → install atomically → chmod. Per-item `[y/N]`
  prompt unless `--yes`. Deps that are `manual`-only are skipped with their hint
  printed (never installed). Prints a summary (installed / skipped / failed).

## §9 · Import / run integration

- **After `fleet import` / agent import:** run `doctor` and print the missing-deps
  report. **Do not block** — the artifact may run degraded, and imports already
  clear egress for safety (a missing render engine is one more thing to grant/
  install locally). Suggest `install-deps`.
- **Before `deep-research run` / `fleet run` / agent start:** `doctor` preflight
  → a non-fatal warning listing missing deps. Runtime already degrades
  gracefully (deep-research on tier-1 search/fetch; the gateway's render-engine
  preflight already returns `*Missing` and the worker marks results unverified).

## §10 · Data model & units

Each unit has one responsibility and a defined interface:

- **`ProgramDep`** (`mur-common`) — the declaration: `name`, `detect: DetectMethod`,
  `reason`, `hint: Option<String>`, `registry: Option<String>`. `DetectMethod` =
  enum `{ File(String), Command(String), Version{command, min} }`. Serde-parsed
  from `requires_programs`. Pure data.
- **`detect`** (`mur-common` or `mur-core`) — `fn detect(dep, mur_home) ->
  DepStatus { Present, Missing, PresentWrongVersion }`. Cross-platform, pure/IO
  only for the check.
- **`curated_registry`** (`mur-common`, shipped manifest + typed accessor) —
  `fn recipe(key, os, arch) -> Option<CuratedRecipe>`. Owns URLs+checksums.
- **`installer`** (`mur-core`) — `fn install(recipe, mur_home, consent) ->
  Result<Installed>`: download, SHA-256 verify, atomic write, chmod, archive
  extract. No knowledge of *which* deps to install — the caller (doctor/
  install-deps) decides.
- **`deps::aggregate`** (`mur-core`) — collect `ProgramDep`s across an agent's
  own `profile.yaml` + its skills + its MCP entries (incl. synthesized
  MCP-command deps), or a fleet's own `fleet.yaml` + its members. One place, so
  agent and fleet share it; dedup by `name`.
- **`doctor` / `install-deps` commands** (`mur-core/src/cmd`) — thin CLI over
  aggregate + detect + installer.
- **Phase 2:** `publisher_recipe` verify path reusing `publisher_trust.rs`.

## §11 · Security considerations

- Curated URLs/checksums are MUR-owned → a malicious bundle can only *reference*
  a key, resolving to MUR's vetted artifact (cannot substitute the URL).
- Every download is SHA-256-verified against the pinned value before it is
  placed or made executable; a mismatch aborts that dep (fail-closed) and
  reports it.
- Phase 2 author recipes require both a valid publisher signature (integrity +
  attribution) AND an explicit user trust-tier elevation (authorization);
  neither alone suffices. Per-install consent surfaces url/sha/target.
- `install-deps` writes only to `install_to` targets under `mur_home` (or an
  allowlisted location); it never runs an arbitrary script from a bundle.
- Detection subprocesses (`version`) are bounded and their output is treated as
  data, never executed.
- A `--deny-host` note: some programs phone home (e.g. Lightpanda's
  `telemetry.lightpanda.io`); the report links that to the egress deny mechanism
  rather than solving it here.

## §12 · Phasing

**Phase 1 (this spec's implementation plan):**
1. `ProgramDep` + `DetectMethod` types + serde on `requires_programs`
   (skill.yaml, `McpServerEntry`, agent `profile.yaml`, `fleet.yaml`).
2. Cross-platform `detect`.
3. Curated registry manifest + accessor; seed `lightpanda` + `obscura`.
4. `installer` (download + SHA-256 + atomic write + chmod + archive extract).
5. `deps::aggregate` (agent skills + MCP-command synthesis + fleet members).
6. `mur agent doctor` extension + `mur fleet doctor` + `install-deps`.
7. Import/run preflight integration (non-blocking report).

**Phase 2 (separate spec/plan):**
- Signed publisher `program_recipe`, trust-tier-gated auto-install, reusing
  `publisher_trust.rs` + Ed25519 + per-install consent.

## §13 · Testing strategy

- `ProgramDep` serde round-trips; each `DetectMethod` parse.
- `detect`: file present/absent, command on/off PATH (with a fake exec dir),
  version parse + min compare, Windows `.exe` suffix (unit-level, `cfg`-guarded).
- `curated_registry`: key→recipe per `(os,arch)`; unknown key → None.
- `installer`: SHA-256 match installs; mismatch fails-closed (no file written);
  atomic temp+rename; archive extract with per-member verify. Use a local
  file:// or a stubbed fetcher — no network in tests.
- `aggregate`: dedup across skills+MCP+members; MCP-command synthesis.
- `doctor` report shape (missing vs present, tier labels); `install-deps`
  skips manual-only deps.
- Import/run integration: missing dep → warning, NOT a hard error.

## §14 · Open questions (resolved defaults)

- **npm/brew-style deps** (agent-browser): out of the download-and-verify
  installer in Phase 1 → surfaced as `manual` hints. A future recipe `kind:
  npm|brew` could wrap them, but not now (YAGNI).
- **Version upgrades:** Phase 1 installs when *missing*; `PresentWrongVersion`
  is reported but not auto-upgraded (avoid clobbering a user's binary silently).
