# MUR Official Catalog — Publish Pipeline (Phase 1) — Design

**Status:** Design / spec
**Date:** 2026-07-21
**Builds on:** `docs/superpowers/specs/2026-07-20-official-catalog-design.md` (overall catalog design), the shipped **client side** (PR #738: `OfficialLicense`, `distribution` marker, import gates, license store, catalog client, `mur official list|install`), quill P2.2 registry-CI signing (reference model), `mur agent export` / `mur fleet export` bundle assembly, `mur_common::muragent::writer` build-from-parts API.

## 1. Goal

Stand up the **official side** of the catalog: a private repo + CI pipeline that turns reviewed source into a signed, `distribution: official` bundle in permanent storage — with **no publish tool shipped in the `mur` binary**. Phase 1 produces the **first real official bundle** using the real official key, plus a **contract draft** that pins the storage/index/license interface so the later mur-server work has a stable seam.

Non-goals for Phase 1 (deferred to the convergence cycle): the mur-server `catalog`/`download`/`license` endpoints, the Hub GUI store, license-signing at download time, version-downgrade/rollback protection, revocation lists.

## 2. Sequencing (hybrid, decided during brainstorm)

1. **Now:** private repo `mur-run/official-catalog` + CI-only signing binary → first real official-marked, official-signed bundle → uploaded to private permanent storage. Verified by CI (in-job signature + fingerprint check) and locally (the PR #738 import gate: no license ⇒ refused; test license ⇒ installs).
2. **Alongside:** the §6 **contract draft** (storage path format, `index.json` schema, license response shape) written into this spec so the CI upload already follows the path convention and future server work has a shared interface.
3. **Later (separate cycle):** converge to the full pipeline design (incl. download-time license signing) + build the mur-server endpoints in parallel. Key-rotation forward-compat (§7) is roadmapped.

**No fake key.** The trust root is exercised with the real official key from the start; test keys appear only in PR dry-runs and unit tests, never in a published artifact.

## 3. Private repo layout — `mur-run/official-catalog`

Store **reviewable plaintext source**, never opaque exported tarballs:

```
agents/<name>/
  profile.yaml            # sanitized agent profile (no secrets, no identity.key)
  prompt.md               # system prompt
  skills/<skill>/...       # bundled skills (source form)
  mcp.yaml                # MCP server refs (command basenames only)
  icon.(png|icns|ico)     # optional
fleets/<name>/
  fleet.yaml
  members/<member>/...     # member profiles + fleet-scoped skills (source form)
catalog.yaml              # SINGLE SOURCE OF TRUTH for metadata (see §6.1)
tools/official-sign/       # CI-only Rust binary (§4), depends on mur-common
.github/workflows/publish.yml
```

- Publish permission = repo write permission; every release is a reviewed PR.
- The private key never lives in the repo — only in a protected GitHub Environment secret (§5).
- The public `mur-run/skill-registry` is unaffected; paid content never lives in any public repo.

## 4. CI-only signing binary — `tools/official-sign`

A standalone `cargo` binary that exists **only in the private repo** and depends on `mur-common` (crates.io release). Nothing in the shipped `mur` binary gains publish capability (the "no leakable tool" decision).

**It builds from source; it does not re-sign an author tarball.** Reviewability comes from CI building the bundle out of the plaintext source the PR reviewed:

- **Muragent:** read `profile.yaml` → `mur_common::muragent::writer::build_manifest_from_profile(profile, mur_version)` → set `manifest.distribution = Some("official")` → drive `MuragentWriter::new(manifest, profile_yaml, official_identity)` with `add_skill` / `add_icon` / `set_sys_prompt` from the source files → `write(out)`. The `official_identity` is the official Ed25519 key loaded from the CI secret; the package is DSSE-signed by it.
- **Fleet:** assemble `fleet.yaml` + member profiles + fleet-scoped skills into a `BundleManifest` with `distribution: Some("official")`, pin entries by `content_hash`, then `manifest_sign_input` + sign with the official key (mirrors `cmd/fleet/export.rs`, which already exposes the primitives).

Output guarantees, asserted in-job before upload:
1. The bundle re-verifies with `mur-common`'s verify path (fleet `verify_manifest_sig`; muragent `validator::validate`).
2. The signer fingerprint equals the pinned `MUR_OFFICIAL_PUBLISHER_KEY_FP` (`ed25519-861d2acb`) — a build signed by the wrong key fails the job.
3. The manifest `name`/`version` match the `catalog.yaml` entry (no metadata drift).

## 5. CI pipeline — `.github/workflows/publish.yml`

- **On PR:** build every changed item with a **throwaway test key**, run the §4 assertions against structure only, and post the expanded bundle contents (manifest + file list + prompt + skill names) as a check summary for human review. The real secret is **never** exposed to PR-triggered runs.
- **On merge-to-main:** a job gated by a protected **GitHub Environment** (required reviewers, pinned action SHAs, least-privilege `GITHUB_TOKEN`) that:
  1. Builds + signs with the real official key (Environment secret).
  2. Verifies (the §4 assertions).
  3. **Immutable upload** (§6.2): refuse if the target `official/<kind>s/<name>/<version>/` path already exists; new content requires a new version.
  4. Regenerates `official/index.json` (§6.3) from `catalog.yaml` + upload results.
  5. Emits SLSA provenance via `actions/attest-build-provenance` binding the bundle to the source commit + CI run (complements the in-bundle DSSE content signature).

## 6. Contract draft (the Phase-1 interface deliverable)

Two interfaces, deliberately separated so neither drifts:

| Interface | Owner | Shape |
|---|---|---|
| **CI → storage** (lands this cycle) | this spec | private storage; paths + `index.json` below |
| **server → client** (frozen by PR #738) | shipped | `GET /api/v1/core/catalog` → `{"items":[{id,tier,version,description}]}`; `/download` → `{"license":{…},"bundle_base64":"…"}` |

The future server **translates** storage `index.json` → the frozen client API; it does not invent a new client contract.

### 6.1 `catalog.yaml` (source of truth)

```yaml
items:
  - id: fleets/deep-research      # "<kind>/<name>", matches client CatalogItem.id
    kind: fleet                    # agent | fleet
    name: deep-research
    version: 1.0.0
    tier: pro                      # free | pro
    description: "…"
```

### 6.2 Storage paths (private bucket, immutable)

```
official/<kind>s/<name>/<version>/bundle.<ext>   # ext: fleet | muragent
official/index.json                              # regenerated each publish
```

- Private bucket. **Bundles are never served to clients directly** — the server reads bytes and returns them inline as `bundle_base64` (per PR #738), so storage stays fully private and pro bytes are never publicly fetchable. `index.json` therefore stores a **storage key**, never a public URL.
- Versioned paths are **write-once**.

### 6.3 `index.json` schema

A JSON array; each item carries everything the server needs to populate the client's `CatalogItem` plus transport metadata:

```json
[
  {
    "id": "fleets/deep-research",
    "kind": "fleet",
    "name": "deep-research",
    "version": "1.0.0",
    "tier": "pro",
    "description": "…",
    "storage_key": "official/fleets/deep-research/1.0.0/bundle.fleet",
    "sha256": "…",
    "size": 12345
  }
]
```

**`sha256` is transport-integrity only, not the trust root.** The trust root remains the DSSE/Ed25519 official signature *inside* the bundle, which the PR #738 import gate verifies. A corrupt `sha256` means "re-fetch"; a bad signature means "refuse to install."

### 6.4 License response shape (recorded now, implemented later)

The future `/download` returns the frozen PR #738 shape:

```json
{ "license": { /* OfficialLicense JSON, signed by the official key */ },
  "bundle_base64": "…" }
```

`OfficialLicense` fields are fixed by mur-common: `format_version, user_id, item, version, expires_at, signer_pubkey, sig`. `expires_at` = subscription period end + grace; checked at download only, never at runtime (local-first).

## 7. Key management & rotation

- **Storage:** Phase 1 keeps the official private key in a protected GitHub Environment secret. A KMS/keyless (sigstore) upgrade is a roadmap item, not a Phase-1 blocker.
- **Rotation forward-compat (roadmap, flagged):** the client pins a **single** `MUR_OFFICIAL_PUBLISHER_KEY_FP` compiled in. Rotating the official key would orphan re-verification of already-installed content. A small follow-up should evolve the client pin from one fingerprint to a **set** (allowing an overlap window during rotation). Recorded here so Phase 1 doesn't foreclose it.

## 8. Threats accepted / out of scope (Phase 1)

- No download-time entitlement/license signing yet (that's the server cycle) — Phase 1 only proves the *bundle* trust chain end to end.
- Version-downgrade/rollback protection and revocation lists deferred (as in the parent spec).
- KMS/keyless signing deferred; GitHub Environment secret is the Phase-1 custody.
- A modified client can still skip license checks — accepted by the parent design.

## 9. Testing

- **Unit (`tools/official-sign`):** manifest stamped before signing; output re-verifies; fingerprint == official fp; stripping the marker breaks the signature; `catalog.yaml` mismatch fails.
- **CI dry-run (PR):** structure-only build with a test key, no upload, no real secret — self-tests the pipeline without touching the trust root.
- **First real bundle (end-to-end):** after the merge-to-main job publishes the first bundle, fetch it and exercise the PR #738 gate locally — **no license ⇒ refused**, matching test license ⇒ installs — proving the published artifact is genuinely official and the anti-sharing gate is live.
