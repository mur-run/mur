# Quill P2.1 — Publisher trust-roots, TOFU pinning & rug-pull drift detection

**Status:** Design / spec
**Date:** 2026-06-28
**Codename:** quill (skills sibling of feather)
**Builds on:** quill C / P2 (merged #531) — `skill_verify.rs` (DSSE/Ed25519 verify-on-install + content-hash), `skill_registry_add.rs` (per-agent install, fail-closed gate), `SkillTrustStore`.

---

## 1. Problem

P2's verify-on-install proves a signature is **internally consistent** (the manifest was signed by the key embedded in its own DSSE envelope) — `SignatureStatus::Verified` means "self-signed correctly", **not** "signed by someone you trust". An attacker can mint their own Ed25519 key, sign a malicious skill with it, and it shows `Verified ✓`. The cosign rule from the research applies directly: **verifying that *a* key signed is insufficient — you must verify *which* identity signed.** P2.1 adds that missing layer, plus rug-pull/drift defense and the registry-side signing companion.

## 2. Decision (research-grounded)

Adopt **pinned-root + TOFU + drift detection** — NOT full TUF, NOT Sigstore keyless.

### Why (deep-research 2026; primary sources)

- **TUF** (root/targets/snapshot/timestamp roles, offline root, rollback/freeze protection) is the gold standard but **too heavy** for a curated git registry — 4 signing roles + metadata management + key ceremony. [TUF spec]
- **Sigstore keyless** (Fulcio short-lived certs + Rekor transparency log + OIDC) **requires network at verify time** — breaks MUR's local-first / offline-capable requirement. Verification must also assert *both* identity *and* issuer. [cosign / Sigstore docs]
- **Trusted Publishing** (npm/crates.io/PyPI: OIDC short-lived, workflow-bound CI credentials + auto provenance) is the right model for the **registry side** (how skills get *into* the repo) — and maps naturally onto MUR's git registry (PR + GitHub identity + CI). [crates.io RFC 3691, npm trusted publishers, SLSA]
- **Content-hash pinning alone cannot distinguish a legitimate update from a malicious one** — that's exactly why a signer-identity layer is needed on top. [npm/OWASP]
- **Pinned-key + TOFU** is the mature, offline, simple model (SSH `known_hosts`, apt keyrings). It reuses MUR's existing Ed25519/DSSE stack and adds the signer-identity layer that hash-pinning lacks.

The verdict: a **client-pinned publisher keyring** (offline root of trust) + **TOFU** for unknown-but-valid publishers + **drift detection** (pin content-hash *and* signer at install, re-prompt on change) + **rollback protection** (monotonic version) — with registry-side CI signing as the companion.

## 3. Design

### 3.1 Publisher keyring (the trust root) — mur-common

A client-side keyring of trusted publisher identities, `mur-common/src/skill/publisher_trust.rs`:
- `PublisherKeyring { schema_version: u32, publishers: Vec<TrustedPublisher>, revoked: Vec<String> }` where `TrustedPublisher { name: String, key_fp: String, comment: String }` and `revoked` holds revoked `key_fp`s.
- Loaded from `~/.mur/trust/publishers.yaml`; **seeded on first run** with the bundled MUR-official publisher key (a const `MUR_OFFICIAL_PUBLISHER_KEY_FP` compiled into the client — the pinned root). Users/teams may add entries.
- `fn classify(keyring, key_fp) -> PublisherTrust` → `Trusted` (in `publishers`, not in `revoked`) / `Revoked` / `Unknown`.

### 3.2 Three-state signature trust (extend P2's `SignatureStatus`)

`skill_verify::verify_skill_install` already returns `SignatureStatus::Verified { publisher, key_fp }`. Add a trust classification on top (do NOT change the cryptographic check):
- New `SignerTrust { Trusted, Untrusted, Revoked, Unsigned, Invalid }` computed by combining `SignatureStatus` with `PublisherKeyring::classify(key_fp)`:
  - `Verified` + key in keyring → `Trusted`
  - `Verified` + key unknown → `Untrusted` (TOFU candidate)
  - `Verified` + key revoked → `Revoked`
  - `Invalid`/`Unsigned` unchanged.
- `verify_skill_install` gains a keyring parameter (or a sibling `classify_signer(outcome, &keyring)`), so the gate can distinguish "signed by a trusted publisher" from "signed by a stranger".

### 3.3 Gate (extend P2's two-tier, fail-closed)

The P2 gate stays; P2.1 refines the `needs_ack` tier and adds `Revoked` to the unconditional-block tier:
- **Unconditional block** (not `--yes`-overridable): hash `Mismatch`, signature `Invalid`, **or signer `Revoked`** (a revoked key = proven-bad).
- **Needs ack** (`--yes`): `Untrusted` signer (TOFU — valid sig, unknown publisher), `Unsigned`, absent hash, scan findings.
- **Clean** (no prompt): hash `Match` + signer `Trusted` + no scan findings.

### 3.4 TOFU pinning

When a user installs an `Untrusted`-but-`Verified` skill with `--yes`, **offer to pin** that publisher: write `{name, key_fp}` into `publishers.yaml` (CLI: a follow-up `mur agent skill trust-publisher <key_fp> [--name]` or an interactive prompt; Hub: a "Trust this publisher" checkbox in consent). Subsequent installs from that publisher are `Trusted`. First-use pins it; later changes are caught by §3.5.

### 3.5 Rug-pull / drift detection (on update / reinstall)

Pin **both** `content_sha256` and `signer key_fp` at install into `SkillTrustStore::TrustEntry` (add `content_sha256: String` and `signer_key_fp: Option<String>` fields — they don't exist today). On `mur skill update` / re-`registry-add`:
- Recompute; if `content_sha256` OR `signer_key_fp` differs from the pinned value → **re-prompt** ("This skill's content/publisher changed since you installed it: <old> → <new>. Reinstall?"), fail-closed without `--yes`.
- **Rollback protection:** reject a registry `latest` whose semver is **lower** than the installed version (monotonic — TUF's cheap rollback defense), unless `--yes`.

### 3.6 Registry-side signing (companion, repo-side — separate task)

In `mur-run/skill-registry` CI (documented, not built here): on PR merge, GitHub identity authenticates the publisher (CODEOWNERS / PR author); CI computes `content_sha256` into `index.yaml` and signs each manifest's DSSE envelope with the **MUR-official key** (whose fingerprint the client pins). This is the "trusted publishing" model adapted to a git registry. Sigstore keyless is a *future* option (deferred — needs online verify).

## 4. Data model

- New: `~/.mur/trust/publishers.yaml` (`PublisherKeyring`).
- Extend `TrustEntry` (mur-common/src/trust/skills.rs) with `content_sha256: String` (+ `#[serde(default)]`) and `signer_key_fp: Option<String>` — for drift detection. Backward-compatible (defaults).
- Const `MUR_OFFICIAL_PUBLISHER_KEY_FP` (the pinned root) in mur-common.

## 5. Security model

- **Trust root:** a client-pinned official publisher key (offline; ships with the binary). `Verified` is no longer sufficient — only `Trusted` (in keyring) installs without a prompt.
- **What this stops:** malicious-but-self-signed skills (now `Untrusted`, gated); a compromised registry host swapping a skill (content-hash mismatch → blocked; or signer changes → drift re-prompt); a revoked key (blocked); downgrade/rollback (monotonic version).
- **What it does NOT stop** (documented): a compromised *official* signing key (mitigated by revocation-list shipped with client updates); a malicious skill from a publisher the user explicitly chose to trust (TOFU is trust-on-*first*-use — the user vouched). No transparency log (Sigstore/Rekor) — accepted for local-first.
- Fail-closed throughout; `Revoked` joins the unconditional-block tier.

## 6. UX (avoid alert fatigue)

- `Trusted` + hash match + clean → **silent install** (no prompt) — the common path stays frictionless.
- Only `Untrusted` / `Unsigned` / scan-findings / drift prompt for `--yes` (CLI) or a checkbox (Hub). Consent shows the signer `key_fp` + whether it's the official key.
- Hub consent: a clear badge — `✓ Trusted publisher (MUR official)` / `⚠ Unknown publisher (key abcd…) — trust on install?` / `✗ Revoked` — plus a "Trust this publisher" action for TOFU.

## 7. Testing

- Unit: `PublisherKeyring::classify` (trusted/unknown/revoked); `classify_signer` matrix (Verified×{in-keyring, unknown, revoked} → Trusted/Untrusted/Revoked); gate (Revoked → unconditional block; Untrusted → needs `--yes`; Trusted+match+clean → no prompt); drift (changed hash or signer → re-prompt; rollback version → blocked).
- Integration: fixture registry + fixture keyring → registry-add a trusted-signed skill (silent), an unknown-signer skill (needs `--yes`, TOFU pins), a revoked-signer skill (blocked), then a drifted update (re-prompt).
- Hub: consent renders the trust badge + TOFU action; gate honored.
- Manual/live: extend the P2 fixture with a signed skill from a keyring'd key vs an unknown key vs a revoked key.

## 8. Non-goals

- Full TUF (roles/snapshot/timestamp metadata).
- Sigstore keyless / Rekor transparency log (needs online verify; future option).
- Web-of-trust / key endorsement graphs.
- The registry-repo CI itself (§3.6 is a companion task in `mur-run/skill-registry`).
- Automatic key distribution beyond shipping the pinned root + revocation list with client updates.

## 9. Open questions

- TOFU prompt vs explicit `mur agent skill trust-publisher` only? (Lean: Hub checkbox + CLI explicit command; no silent auto-pin.)
- Ship the revocation list embedded in the client, or fetch a signed `revocations.yaml` from the registry? (Lean: embedded + registry-fetched union, both signed by the pinned root.)
- Should `Untrusted` (valid sig, unknown publisher) require `--yes` *and* a TOFU pin, or just `--yes`? (Lean: `--yes` installs; pinning is a separate opt-in so a one-off install doesn't grow the keyring.)
