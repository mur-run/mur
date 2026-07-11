# Portable Program Dependencies — Phase 2 Design (Trusted-Publisher Recipes)

**Status:** Design approved 2026-07-11. Builds on the Phase 1 spec
(`2026-07-11-portable-program-dependencies-design.md`, §7.2) and its shipped
implementation (PR #684).

## §1 · Problem & goal

Phase 1 auto-installs only **MUR-curated** programs (registry keys MUR owns).
An artifact that needs a program MUR hasn't curated degrades to detect-and-guide
— the recipient installs it manually. Phase 2 lets a **signed bundle from a
trusted publisher** carry an **author-declared install recipe** and auto-install
it, so a self-contained agent/fleet from someone the user trusts "just works" —
without MUR having to curate every program.

The security cost (auto-running an author-declared installer) is bounded by the
**trust gradient** established in Phase 1 §7.2: this path is available **only**
for a publisher the user has explicitly elevated to trusted, and only with
per-install consent. Unknown/untrusted publishers stay detect-and-guide.

## §2 · Core decision — trust is anchored at import (Model A)

A bundle's publisher signature is verifiable **only at import**, when the signed
manifest is present. Once imported, the artifact's `profile.yaml`/`fleet.yaml`
sit unsigned on the user's own disk. Therefore the trusted-publisher install
**happens at import time**, when signature + publisher-trust are both
verifiable. `install-deps` (post-import) stays **curated-only** (Phase 1
unchanged). No post-import trusted-recipe path exists in Phase 2 — that would
require persisting per-recipe provenance (deferred; not chosen).

**Consequence:** if the user declines at import, or no recipe matches the
current platform, the dependency stays missing (doctor later shows it `manual`).
Re-importing re-offers. This is an accepted tradeoff for anchoring trust to the
one moment it is cryptographically verifiable.

## §3 · Global constraints

- **Reuse, don't rebuild.** Reuse Phase 1's `installer::install`
  (download → verify SHA-256 → atomic write → chmod → all-or-nothing extract),
  the `CuratedRecipe` per-platform shape, and `PublisherKeyring::classify`.
- **Cross-platform** — author recipes are keyed by `<arch>-<os>` exactly like
  curated recipes; only the current platform's entry is installed.
- **Integrity + authorization are separate factors, both required:** the
  author's pinned SHA-256 provides download integrity; the **bundle signature +
  `classify == Trusted`** provides authorization. Neither alone triggers an
  install; a **per-install `[y/N]` consent** (showing publisher, url, sha256,
  install target) is the final gate.
- **Fail-closed:** `Revoked` publisher → refuse. `Unknown` → detect-and-guide
  (never auto-install). Signature invalid → the bundle is already refused by the
  existing import path before Phase 2 runs.
- **Curated wins:** if an author recipe's `name` collides with a MUR-curated
  registry key, the curated recipe is authoritative (MUR-owned source beats
  author-declared); the author recipe for that name is ignored.
- **Non-blocking:** a failed or declined trusted-install never fails the import
  (the artifact imports and degrades, per Phase 1's non-blocking contract).
- Files ≤ 800 lines; brand "MUR"; comments/spec English.

## §4 · The `recipe` field on `ProgramDep`

`ProgramDep` (Phase 1) gains one optional field:

```yaml
requires_programs:
  - name: some-tool
    detect: { command: some-tool }
    reason: "what it's for"
    hint: "https://example.com/some-tool"     # still the untrusted fallback
    recipe:                                     # author-declared, per-platform
      aarch64-macos: { url: "https://…", sha256: "…", install_to: "aura/some-tool", executable: true }
      x86_64-linux:  { url: "https://…", sha256: "…", install_to: "aura/some-tool", executable: true }
```

- The per-platform value **reuses the Phase 1 recipe shape** (`url`, `sha256`,
  `install_to`, `executable`, optional `archive: { members }`). Represent it as
  `ProgramRecipe { platforms: BTreeMap<String, PlatformRecipe> }` where
  `PlatformRecipe` deserializes to the same fields the curated `recipe()`
  returns — so it converts cleanly into the `CuratedRecipe` the Phase 1
  `installer::install` already consumes.
- The `recipe` lives inside the artifact YAML, which is inside the tar the
  bundle manifest signs — so its integrity flows from the **existing bundle
  signature**. No separate per-recipe signature is introduced.
- `recipe` is `#[serde(default, skip_serializing_if = "Option::is_none")]` →
  absent for every Phase 1 artifact (back-compat).

## §5 · Trust-gated install at import

Applies symmetrically to **`mur fleet import`** and **agent (`.muragent`)
import** — both already verify the bundle signature and derive the signer
fingerprint.

At import, AFTER the existing signature verification succeeds and the signer
fingerprint (`derived_fp`) is known (`cmd_fleet_import` already computes this via
`signer_fingerprint`):

1. Load the keyring (`PublisherKeyring::load_or_seed(mur_home)`) and
   `classify(&derived_fp)`.
2. Aggregate the imported artifact's `requires_programs` (reuse Phase 1
   `aggregate_*`), and for each dep that (a) is currently **missing** (Phase 1
   `detect`), (b) is **not** a curated key (curated wins → handled by Phase 1
   `install-deps` messaging instead), and (c) has a `recipe` entry for
   `current_platform()`:
   - **`Trusted`** → print the recipe (publisher name, url, sha256, target) and
     `confirm("Install <name> from <url> (signed by <publisher>)?", yes)`; on
     yes, convert `PlatformRecipe` → `CuratedRecipe` and call
     `installer::install(&recipe, mur_home)`. Print installed/failed.
   - **`Unknown`** → do NOT install; print one line:
     `<name>: recipe from an untrusted publisher — run \`mur agent skill signer-trust <fp>\` to trust <publisher>, or install manually: <hint/url>`.
   - **`Revoked`** → do NOT install; print a refusal:
     `<name>: publisher <fp> is REVOKED — not installing.`
3. This whole block is best-effort/non-blocking (wrapped so a failure never
   aborts the import), consistent with Phase 1's import preflight.

`--yes` on import auto-approves the per-item consent (scripted installs the user
opted into), same semantics as Phase 1 `install-deps --yes`.

## §6 · Data model & units

- **`ProgramRecipe` / `PlatformRecipe`** (`mur-common/src/deps/`) — the
  author-declared, per-platform recipe on `ProgramDep`. `PlatformRecipe` mirrors
  the curated per-platform fields so a single `to_curated()` converter yields the
  `CuratedRecipe` that `installer::install` already takes. One place converts;
  the installer is untouched.
- **`classify` at import** — reuse `mur_common::skill::publisher_trust::PublisherKeyring`
  (`load_or_seed` + `classify`). No new trust store.
- **import trusted-install hook** (`mur-core/src/cmd/fleet/import.rs` + the agent
  import path) — the §5 block. Small; calls existing `aggregate_*`, `detect`,
  `installer::install`, and a local `confirm`.
- Everything else (detect, curated registry, installer, doctor, install-deps) is
  **unchanged** from Phase 1.

## §7 · Security considerations

- **Two required factors:** integrity (author sha256 pins the bytes) AND
  authorization (valid bundle signature already enforced at import + `classify ==
  Trusted`). A malicious bundle from an **unknown** publisher can declare a
  recipe but it is **never auto-installed** — it degrades to the same
  detect-and-guide as an unsigned bundle. A signature only proves *who*; the
  user's explicit keyring elevation is what authorizes *running their installer*.
- **Per-install consent** surfaces publisher + url + sha256 + target before any
  download, so even a trusted publisher's install is a conscious act (unless
  `--yes`).
- **Revocation is honored** (`classify` returns `Revoked` fail-closed, precedence
  over `Trusted`) — a compromised publisher key added to the keyring's `revoked`
  list disables its recipes immediately.
- **No new download/verify code** — the Phase 1 `installer` (sha256-before-write,
  all-or-nothing extract, no tar-path-traversal, writes only to `install_to`
  under `mur_home`) is reused verbatim, so its audited integrity guarantees carry
  over.
- **Trust is not transitive to arbitrary URLs beyond consent:** a trusted
  publisher's recipe can point `url` anywhere, but the user sees it and the sha256
  pins exactly those bytes; the publisher can't swap the artifact after the user
  consents.

## §8 · Testing strategy

- `ProgramRecipe` serde round-trip; `to_curated()` conversion (bare + archive).
- `recipe` absent on a Phase 1 `ProgramDep` → back-compat parse.
- Trust-gate decision table (pure, no I/O): given `(classify, is_curated,
  detect_status, has_platform_recipe)` → one of {offer-install, skip-untrusted,
  skip-revoked, skip-curated, skip-present, skip-no-platform}. Unit-test this
  decision function directly so the import hook stays thin.
- Curated-wins: a dep whose `name` is a curated key is NOT offered via the author
  recipe path.
- Import remains non-blocking: a recipe install failure/decline → import still
  `Ok`.
- Reuse of `installer::install` is covered by Phase 1's installer tests (not
  re-tested).

## §9 · Non-goals (Phase 2)

- Post-import trusted-recipe install via `install-deps` (needs persisted
  provenance — Model B, deferred).
- A separate "program-install" trust tier distinct from the skill-publisher
  keyring (the per-install consent is the second factor; no new tier).
- npm/brew-style recipe kinds (Phase 1 non-goal, still out).
- Signing/among-the-recipe granularity below the bundle signature (the bundle
  signature already covers the recipe).
