# Quill C (P2) — Per-agent skill registry install + verify-on-install + transparent consent

**Status:** Design / spec
**Date:** 2026-06-28
**Codename:** quill (the skills sibling of **feather**, MCP servers)
**Builds on:** the EXISTING git-based skill registry (already shipped) + quill P1/P-bundles (`skill_remote.rs`, `skill_bundle.rs`) + feather's `mur agent mcp registry-add` pattern.

---

## 1. Problem & the actual gap

A skill registry **already exists and is wired** in the codebase — this design extends it, it does **not** build a new one. What exists today:

| Capability | Where |
|---|---|
| Git-repo registry (`github.com/mur-run/skill-registry`, shallow clone → `index.yaml` + `skills/<name>/versions/<semver>.yaml`) | `mur-core/src/cmd/skill_registry.rs` (`DEFAULT_REGISTRY`, `fetch_and_load`, `search_registry`, `available_versions`, `skill_yaml_path`) |
| `mur skill search` (local + registry) | `cmd/skill_cmd.rs::cmd_search` |
| `mur skill install <name>` / `update` (**user-level** `~/.mur/skills/`) | `cmd/skill_install.rs::cmd_install` + `cmd/skill_resolver.rs` |
| Per-manifest DSSE/Ed25519 signing **primitives** | `mur-common/src/skill/sign.rs` (`sign_manifest`, `verify_manifest`, `SKILL_PAYLOAD_TYPE`); manifest carries `publisher_signature: Option<String>` |
| Registry index entry with **content hash** + publisher + trust | `mur-common/src/skill/registry.rs` (`RegistryIndex`, `RegistrySkillEntry { latest, description, publisher, category, tags, content_sha256, install_count }`) |
| Per-agent local install (validate + scan, fail-closed-ish) | `cmd/agent/skill.rs::cmd_skill_add` (writes `agents/<name>/skills/<name>/skill.yaml`) |
| Trust levels | `mur_common::skill::TrustLevel` (Sandboxed default) |

**Two concrete gaps:**

1. **No per-agent registry install.** `mur skill install` targets the **user** store (`~/.mur/skills/`). There is no `mur agent skill registry-add <agent> <name>` to install a registry skill onto a **specific agent** (`agents/<name>/skills/`) — even though feather already shipped the sibling `mur agent mcp registry-add <agent> <server>`.
2. **Signatures and the content hash are never verified on install.** `verify_manifest` (DSSE) has **no callers in `mur-core`** — the signing primitives are unwired. `content_sha256` exists in the index schema but is not checked. Installation runs the schema validation + content scan only.

## 2. Decision (re-scoped, research-grounded)

Bring the existing DSSE-signed git registry to the **per-agent + Hub** surface, and add **verify-on-install (fail-closed) with a transparent consent screen**. Do **not** replace the git registry, and do **not** build TUF/Sigstore.

### Best-practice basis (deep-research 2026; sources cited)

- **A registry is an identity/ownership authority, not a signing authority** — the official MCP registry imposes no crypto signing at the registry layer; trust is per-artifact. → MUR's git registry + per-manifest DSSE is the correct shape; keep it. [modelcontextprotocol.io official-registry-requirements; github.com/modelcontextprotocol/registry — confirmed 3-0]
- **Verify-on-install, fail-closed, bound to a CONTENT HASH** (not to a name/key, which permits silent payload swaps); pin a sha256 at install, alert/re-prompt on change (rug-pull defense). [OWASP MCP Security Cheat Sheet; multiple tool-poisoning sources]
- **A one-time install grant over-grants; verification must be mandatory, not audit-only.** [agent-skill threat analyses]
- **Per-agent identity + least privilege** — assign capability onto a specific agent, install at the lowest trust level, elevate deliberately. [Claude agent-identity model; VS Code Workspace Trust / Restricted Mode]
- **Defer acting on installed content until after consent.** [Anthropic shipped this fix for project settings]

## 3. Design

### 3.1 `skill_registry_add` (mur-core, per-agent) — the headline

New `cmd/agent/skill_registry_add.rs` (sibling of `skill_remote.rs`), mirroring `mcp_registry::cmd_mcp_registry_add(agent, server_name)`:

```rust
pub async fn cmd_skill_registry_add(
    agent: &str,
    name: &str,
    version: Option<&str>,   // None ⇒ latest
    accept: bool,            // --yes: proceed despite a verify/scan failure
) -> Result<String>          // returns "skills/<name>"
```

Flow (reuses existing primitives):
1. `skill_registry::fetch_and_load(home, DEFAULT_REGISTRY)` → `(registry_dir, RegistryIndex)`.
2. Resolve the entry + version (`RegistryIndex.skills[name]`, `available_versions`, `skill_yaml_path`).
3. Read the resolved `skill.yaml` from the clone.
4. **Verify-on-install (§3.2) — fail-closed unless `accept`.**
5. Reuse `cmd::agent::skill::cmd_skill_add(agent, &resolved_path)` to validate + scan + write into `agents/<agent>/skills/`, **installed at `TrustLevel::Sandboxed`**.

### 3.2 Verify-on-install (fail-closed) — the core upgrade

Before the skill is written, in a new `skill_verify` helper used by both the per-agent install and (optionally) the existing user-level `cmd_install`:

```rust
pub struct VerifyOutcome {
    pub hash_ok: bool,                 // file sha256 == entry.content_sha256
    pub signature: SignatureStatus,    // Verified{publisher,key_fp} | Unsigned | Invalid
}
pub enum SignatureStatus { Verified { publisher: String, key_fp: String }, Unsigned, Invalid }
```

- **Content hash:** if `entry.content_sha256` is non-empty, compute the sha256 of the resolved skill file and compare. Mismatch ⇒ `bail!` (fail-closed) unless `accept`. Empty hash ⇒ treat as `Unsigned`-equivalent (warn).
- **Signature:** if `manifest.publisher_signature` is `Some(envelope)`, call `mur_common::skill::verify_manifest(&manifest, &envelope)`. `Err` ⇒ `Invalid` ⇒ `bail!` unless `accept`. `Ok` ⇒ `Verified { publisher, key_fp = fingerprint of the envelope's key }`. `None` ⇒ `Unsigned`.
- **Publisher key pinning is deferred** (P2.1): `verify_manifest` proves the envelope is internally consistent (signed by the key it carries); it does not yet prove that key belongs to a *trusted* publisher. P2 surfaces the publisher + key fingerprint in consent and installs Sandboxed; a pinned trust-root of publisher keys is a follow-on. (Documented as a known limitation, not silently assumed.)

### 3.3 Transparent consent (CLI + Hub)

Before installing, show — and (CLI) require confirmation unless `--yes`:
- **publisher** + **signature status** (`✓ verified (key abcd…)` / `⚠ unsigned` / `✗ invalid`)
- **content-hash** match (`✓` / `✗ mismatch`)
- **trust level** the skill will be installed at (Sandboxed)
- **declared MCP requirements / permissions** (`manifest.mcp_requirements`, if any)
- **scan findings** (`scan_skill`) and the **full skill body** (tool-poisoning defense — the threat lives in the description/content, invisible until shown)
- **defer**: nothing in the skill is parsed-for-effect or installed until consent passes.

### 3.4 CLI surface

- `mur agent skill registry-add <agent> <name> [--version <semver>] [--yes]` — per-agent install (§3.1).
- `mur agent skill search <query> [--refresh]` — registry search scoped for the agent flow (reuse `search_registry`); or document that the existing `mur skill search` covers discovery and `registry-add` is the install verb. (Lean: add `mur agent skill search` for symmetry with `mur agent mcp`.)

### 3.5 Hub surface

Skills tab → **"Browse registry"**: search box → results (name / description / publisher / latest version / signature badge) → **Install onto `<agent>`** → consent modal (§3.3) → install.
- New Tauri commands in `mcp_skills.rs`: `agent_skill_registry_search(query, refresh) -> Vec<RegistryResult>` and `agent_skill_registry_install(agent, name, version, accept) -> AgentDetail` (the install returns the refreshed agent detail like the existing URL-install path).
- Reuse the consent rendering shape from the quill P1/bundle `SkillAddUrlModal` (publisher/signature/findings/body), feeding it registry metadata.

### 3.6 Rug-pull defense (update)

`mur skill update` (and a re-`registry-add`) re-runs §3.2; if the resolved content's hash/signature differs from what is installed, **re-prompt** (do not silently replace). Pin the installed `content_sha256` in `SkillStats`/local metadata so drift is detectable.

## 4. Data model

No new index schema needed — `RegistrySkillEntry.content_sha256` already exists; this design **populates and verifies** it. The registry repo's CI is responsible for writing the correct `content_sha256` per version (server/repo-side concern, noted in §7 companion task). Client adds only: an installed-skill record of the verified `content_sha256` (for §3.6 drift detection) in the existing local skill metadata.

## 5. Security model

- **Verify-on-install, fail-closed** (§3.2): content-hash + DSSE signature checked **before** write; failure aborts unless `--yes`/explicit accept. This is the central upgrade over today's audit-only posture.
- **Least privilege**: registry skills install at `TrustLevel::Sandboxed`; the user elevates deliberately.
- **Transparent consent** (§3.3): publisher, signature, trust, permissions, scan findings, full body shown before install; content not acted on pre-consent.
- **Rug-pull** (§3.6): re-verify on update; re-prompt on drift.
- **Transport**: registry fetched via git over HTTPS (existing). DSSE per-artifact signature is the provenance layer (not the transport).
- **Deferred (documented, not assumed):** publisher-key pinning / trust-roots (P2.1); TUF-style signed index metadata; Sigstore keyless. These are appropriate only if/when MUR moves from a git registry to a hosted central registry.

## 6. Testing

- **Unit (network-free):** `skill_verify` — hash match/mismatch; signature Verified/Unsigned/Invalid via a fixture-signed manifest (reuse `sign_manifest` with a test `AgentIdentity`); fail-closed unless `accept`. Version resolution (latest vs pinned). Consent struct assembly.
- **Integration:** `cmd_skill_registry_add` against a **local fixture registry dir** (a temp dir laid out like the clone: `index.yaml` + `skills/<name>/versions/<v>.yaml`, with a correct + a tampered `content_sha256`, and a signed + unsigned + bad-signature skill) → asserts install onto a scratch agent, and fail-closed on tamper/bad-sig.
- **Hub:** `agent_skill_registry_search` returns results; install wiring compiles; consent modal renders signature/findings.
- **Manual/live:** point `DEFAULT_REGISTRY` (or an override) at a local git repo; `registry-add` a clean signed skill (installs), a hash-tampered one (rejected), an unsigned one (warns, installs only with `--yes`).

## 7. Companion task (separate, repo-side)

The `mur-run/skill-registry` repo's publish/CI must (a) compute and write `content_sha256` per version into `index.yaml`, and (b) carry each skill's `publisher_signature` (DSSE). This is a **separate small task in the registry repo**, not in this plan; P2 ships the client that verifies whatever the registry provides (and degrades to "unsigned/unhashed → warn, require `--yes`" so it works against today's unsigned registry).

## 8. Non-goals

- Replacing the git registry with a central HTTP API / OCI registry.
- TUF, Sigstore keyless signing, in-toto/SLSA provenance.
- Namespace-ownership enforcement (a registry-repo CI / server concern).
- Publisher-key trust-roots / pinning (P2.1 follow-on).
- Ratings / download-count UI beyond the existing `install_count`.
- Changing the user-level `mur skill install`/`update` model (only optionally sharing the new `skill_verify` helper with it).

## 9. Open questions

- Should `mur agent skill registry-add` install onto the agent's per-agent store **or** install user-level + scope to the agent? (Lean: per-agent store, mirroring `mur agent mcp registry-add`, for true per-agent identity.)
- Surface `mur agent skill search` as a distinct verb, or rely on the existing `mur skill search` for discovery? (Lean: add it for symmetry.)
- Populate `content_sha256` verification as **required** once the registry repo emits hashes, or keep "unsigned/unhashed → warn + `--yes`" indefinitely for third-party registries? (Lean: required when present; warn when absent.)
