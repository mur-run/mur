# Team-Shared Fleets — Phase A Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a signed, portable `.fleet` bundle plus `mur fleet export` / `mur fleet import`, so a fleet's definition + its fleet-scoped skills (and optionally its member agents) can move between machines/people, local-first, under a fail-closed two-tier trust model.

**Architecture:** `export` collects `fleet.yaml` + the fleet's `scope:Fleet` skills (+ optional member agent exports), pins each file by SHA-256 in a `BundleManifest`, signs the manifest with the local concierge identity, and writes a `tar.gz` `.fleet` via a pluggable transport (`LocalFile`). `import` reads the bytes, verifies the manifest signature + every file hash, security-scans each skill, requires HITL confirmation, then installs skills (at lowest trust, `scope:Fleet`) and the fleet, reporting missing members. Build/parse is decoupled from transport so server sync slots in later.

**Tech Stack:** Rust edition 2024. New code in `mur-common` (pure bundle types + crypto helper) and `mur-core` (`cmd/fleet/{export,import,bundle_transport}.rs`). Reuse: `mur-common::identity::AgentIdentity` (Ed25519), `mur-common::skill::{local,store,scan,manifest,types}`, `mur-core::cmd::fleet::store`, `mur-core::cmd::fleet_sync` (profile-minus-key assembly), `a2a_dial::canonicalize_agent_name`. Archive via `tar` + `flate2` (gzip), already workspace deps (used by `mur-agent-runtime/src/export/pkg.rs`).

## Global Constraints

- Brand: user-facing text uppercase **MUR**; CLI/`name`/paths lowercase.
- No hardcoded magic values: use named constants (`FLEET_BUNDLE_FORMAT`, etc.).
- `mur-common` is types-only (pure); all filesystem I/O lives in `mur-core`.
- Bundle is **untrusted observed data, never commands**: verify-before-install; **provenance ≠ trust** (imported skills land at the lowest trust tier regardless of any `trust:` they claim); import **never auto-runs** the fleet.
- Fail-closed: any signature or file-hash mismatch → refuse with no partial install; unsigned → refuse unless `--force`.
- Signing identity: the local concierge agent identity at `<mur_home>/agents/mur/` (`identity.key`/`identity.pub`); never copy private keys into a bundle.
- Source files ≤ 800 lines.
- Lowest trust tier for imported skills = `TrustLevel::Sandboxed` (the `#[default]`).
- Skills scope enum lives in `mur_common::skill::manifest::SkillScope` (`User|Project|Fleet|Enterprise`).

---

## File Structure

| File | New/Mod | Responsibility |
|---|---|---|
| `mur-common/src/fleet_bundle.rs` | **New** | `BundleManifest`, `BundleEntry`, `FLEET_BUNDLE_FORMAT`, `content_hash`, `manifest_sign_input`, `verify_manifest_sig`, `signer_fingerprint` — pure types + canonical bytes. No I/O. |
| `mur-common/src/lib.rs` | Mod | `pub mod fleet_bundle;` |
| `mur-common/src/identity.rs` | Mod | add `verify_bytes(pubkey, msg, sig_multibase) -> bool` (generic Ed25519 verify, fail-closed). |
| `mur-core/src/cmd/fleet/bundle_transport.rs` | **New** | `FleetBundleTransport` trait + `LocalFile` impl (the seam). |
| `mur-core/src/cmd/fleet/export.rs` | **New** | `cmd_fleet_export` + `collect_fleet_skills` + tar.gz build + manifest sign. |
| `mur-core/src/cmd/fleet/import.rs` | **New** | `cmd_fleet_import` + `ImportOpts` + extract + verify + scan + HITL + install + member handling. |
| `mur-core/src/cmd/fleet/mod.rs` | Mod | declare `export`, `import`, `bundle_transport` modules. |
| `mur-core/src/cli/actions.rs` | Mod | add `Export`/`Import` variants to `FleetAction`. |
| `mur-core/src/dispatch.rs` | Mod | dispatch the two new variants. |
| `mur-core/Cargo.toml` | Mod | add `tar` + `flate2` deps if absent. |
| `CLAUDE.md` | Mod | one line: fleet export/import shipped. |

---

## Task 1: Bundle types + manifest signing primitives (`mur-common`)

**Files:**
- Create: `mur-common/src/fleet_bundle.rs`
- Modify: `mur-common/src/lib.rs` (add `pub mod fleet_bundle;`), `mur-common/src/identity.rs` (add `verify_bytes`)
- Test: inline `#[cfg(test)] mod tests` in `fleet_bundle.rs`

**Interfaces:**
- Produces:
  - `pub const FLEET_BUNDLE_FORMAT: u32 = 1;`
  - `pub struct BundleEntry { pub path: String, pub sha256: String }`
  - `pub struct BundleManifest { pub format_version: u32, pub fleet_name: String, pub created_at: String, pub signer_pubkey: String, pub signer_fingerprint: String, pub includes_members: bool, pub members: Vec<String>, pub entries: Vec<BundleEntry>, pub sig: Option<String> }`
  - `pub fn content_hash(bytes: &[u8]) -> String` (lowercase hex SHA-256)
  - `pub fn manifest_sign_input(m: &BundleManifest) -> Vec<u8>` (canonical JSON of the manifest with `sig=None`)
  - `pub fn signer_fingerprint(signer_pubkey_multibase: &str) -> String`
  - `pub fn verify_manifest_sig(m: &BundleManifest, pubkey: &[u8; 32]) -> bool`
  - `mur_common::identity::verify_bytes(pubkey: &[u8; 32], msg: &[u8], sig_multibase: &str) -> bool`
- Consumes: `AgentIdentity::{sign_bytes, verifying_key_bytes, public_key_multibase, generate}` (existing, `mur-common/src/identity.rs`).

- [ ] **Step 1: Add the generic verify helper to identity.rs**

In `mur-common/src/identity.rs`, add (near the existing signing code; `ed25519_dalek` and `multibase` are already used in this file):

```rust
/// Verify a multibase-encoded Ed25519 signature over `msg` against `pubkey`.
/// Fail-closed: any decode/length/verify error returns false.
pub fn verify_bytes(pubkey: &[u8; 32], msg: &[u8], sig_multibase: &str) -> bool {
    let Ok((_, sig_bytes)) = multibase::decode(sig_multibase) else {
        return false;
    };
    let Ok(sig_arr): Result<[u8; 64], _> = sig_bytes.try_into() else {
        return false;
    };
    let Ok(vk) = ed25519_dalek::VerifyingKey::from_bytes(pubkey) else {
        return false;
    };
    vk.verify_strict(msg, &ed25519_dalek::Signature::from_bytes(&sig_arr))
        .is_ok()
}
```

(If `use ed25519_dalek::Verifier;` or `Signer` is needed for `.verify_strict`, add it; check the existing imports in identity.rs and match how `sign_bytes` references the crate.)

- [ ] **Step 2: Write the failing test for fleet_bundle**

Create `mur-common/src/fleet_bundle.rs` with ONLY the test module first (so it fails to compile / fails):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::AgentIdentity;

    fn sample_manifest() -> BundleManifest {
        BundleManifest {
            format_version: FLEET_BUNDLE_FORMAT,
            fleet_name: "devteam".into(),
            created_at: "2026-06-20T00:00:00Z".into(),
            signer_pubkey: String::new(),
            signer_fingerprint: String::new(),
            includes_members: false,
            members: vec!["pm".into(), "qa".into()],
            entries: vec![BundleEntry {
                path: "fleet.yaml".into(),
                sha256: content_hash(b"name: devteam\n"),
            }],
            sig: None,
        }
    }

    #[test]
    fn content_hash_is_deterministic_hex() {
        assert_eq!(content_hash(b"abc"), content_hash(b"abc"));
        assert_ne!(content_hash(b"abc"), content_hash(b"abd"));
        assert_eq!(content_hash(b"").len(), 64); // sha256 hex
    }

    #[test]
    fn manifest_roundtrips_yaml() {
        let m = sample_manifest();
        let y = serde_yaml::to_string(&m).unwrap();
        let back: BundleManifest = serde_yaml::from_str(&y).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn sign_then_verify_roundtrip_and_tamper_fails() {
        let id = AgentIdentity::generate();
        let mut m = sample_manifest();
        m.signer_pubkey = id.public_key_multibase();
        m.signer_fingerprint = signer_fingerprint(&m.signer_pubkey);
        // sign canonical input (sig must be None during signing)
        let input = manifest_sign_input(&m);
        m.sig = Some(multibase::encode(
            multibase::Base::Base58Btc,
            id.sign_bytes(&input),
        ));
        let pubkey = id.verifying_key_bytes();
        assert!(verify_manifest_sig(&m, &pubkey));

        // tamper an entry hash → verify fails
        let mut tampered = m.clone();
        tampered.entries[0].sha256 = content_hash(b"evil");
        assert!(!verify_manifest_sig(&tampered, &pubkey));

        // flip the signature → verify fails (fail-closed)
        let mut badsig = m.clone();
        badsig.sig = Some(multibase::encode(multibase::Base::Base58Btc, [0u8; 64]));
        assert!(!verify_manifest_sig(&badsig, &pubkey));

        // missing signature → false
        let mut unsigned = m.clone();
        unsigned.sig = None;
        assert!(!verify_manifest_sig(&unsigned, &pubkey));
    }

    #[test]
    fn fingerprint_is_short_and_stable() {
        let fp = signer_fingerprint("zSomePubKey");
        assert_eq!(fp, signer_fingerprint("zSomePubKey"));
        assert!(fp.len() <= 12 && !fp.is_empty());
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-common -E 'test(/fleet_bundle/)'`
Expected: FAIL — `BundleManifest` / `content_hash` / etc. not defined (won't compile).

- [ ] **Step 4: Implement the bundle types + functions**

Prepend to `mur-common/src/fleet_bundle.rs` (above the test module):

```rust
//! `.fleet` bundle manifest types + signing primitives (pure; no I/O).
//!
//! The MANIFEST is the only signed object: it pins every bundled file by
//! SHA-256, so the archive container need not be byte-deterministic — verifying
//! each file's hash against the manifest plus the manifest signature suffices.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Bundle wire-format version. Bump on any breaking manifest change.
pub const FLEET_BUNDLE_FORMAT: u32 = 1;

/// One file in a bundle, pinned by content hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleEntry {
    /// Bundle-relative path, e.g. `fleet.yaml`, `skills/triage/skill.yaml`.
    pub path: String,
    /// Lowercase hex SHA-256 of the file's bytes.
    pub sha256: String,
}

/// The signed manifest at `bundle.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleManifest {
    pub format_version: u32,
    pub fleet_name: String,
    pub created_at: String,
    /// Exporter's concierge identity public key (multibase).
    pub signer_pubkey: String,
    /// Short, human-checkable fingerprint of `signer_pubkey`.
    pub signer_fingerprint: String,
    pub includes_members: bool,
    /// Declared member names (always listed, even when not bundled).
    pub members: Vec<String>,
    /// Every bundled file pinned by hash.
    pub entries: Vec<BundleEntry>,
    /// Multibase Ed25519 signature over `manifest_sign_input` (this manifest
    /// with `sig=None`). `None` only while building, before signing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig: Option<String>,
}

/// Lowercase hex SHA-256 of `bytes`.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// Canonical signing input: the manifest serialized with `sig` cleared. Struct
/// field order is fixed, so this is deterministic without a custom canonicalizer.
pub fn manifest_sign_input(m: &BundleManifest) -> Vec<u8> {
    let mut unsigned = m.clone();
    unsigned.sig = None;
    serde_json::to_vec(&unsigned).expect("manifest serializes")
}

/// Short fingerprint of a multibase pubkey: first 8 hex chars of its SHA-256,
/// hyphen-grouped (e.g. `ab12-cd34`) for human out-of-band comparison.
pub fn signer_fingerprint(signer_pubkey_multibase: &str) -> String {
    let h = content_hash(signer_pubkey_multibase.as_bytes());
    format!("{}-{}", &h[0..4], &h[4..8])
}

/// Verify the manifest signature against `pubkey`. Fail-closed: no `sig` → false.
pub fn verify_manifest_sig(m: &BundleManifest, pubkey: &[u8; 32]) -> bool {
    let Some(sig) = m.sig.as_deref() else {
        return false;
    };
    crate::identity::verify_bytes(pubkey, &manifest_sign_input(m), sig)
}
```

Add `pub mod fleet_bundle;` to `mur-common/src/lib.rs` (alphabetically near `pub mod fleet;`).

Confirm `hex`, `sha2`, `serde_json`, `serde_yaml`, `multibase` are deps of `mur-common` (they are — used across the crate; if `hex` is missing, add it).

- [ ] **Step 5: Run the tests to verify they pass**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-common -E 'test(/fleet_bundle/)'`
Expected: PASS — 4 tests.

- [ ] **Step 6: Lint + commit**

Run: `cargo fmt -p mur-common && ORT_STRATEGY=download cargo clippy -p mur-common --no-deps -- -D warnings`
```bash
git add mur-common/src/fleet_bundle.rs mur-common/src/lib.rs mur-common/src/identity.rs
git commit -m "feat(fleet): .fleet bundle manifest types + sign/verify primitives"
```

---

## Task 2: `mur fleet export` (core, no members)

**Files:**
- Create: `mur-core/src/cmd/fleet/bundle_transport.rs`, `mur-core/src/cmd/fleet/export.rs`
- Modify: `mur-core/src/cmd/fleet/mod.rs`, `mur-core/src/cli/actions.rs`, `mur-core/src/dispatch.rs`, `mur-core/Cargo.toml`
- Test: inline tests in `export.rs`

**Interfaces:**
- Consumes (Task 1): `mur_common::fleet_bundle::{BundleManifest, BundleEntry, FLEET_BUNDLE_FORMAT, content_hash, manifest_sign_input, signer_fingerprint}`; `mur_common::identity::AgentIdentity`.
- Consumes (existing): `crate::cmd::fleet::store::{load_fleet, fleets_dir}`; `mur_common::skill::local::{list_installed, load_installed}`; `mur_common::skill::store::global_skill_dir`; `mur_common::skill::manifest::{SkillManifest, SkillScope}`.
- Produces:
  - `pub trait FleetBundleTransport { fn read(&self, src: &str) -> anyhow::Result<Vec<u8>>; fn write(&self, dst: &str, bytes: &[u8]) -> anyhow::Result<()> }`
  - `pub struct LocalFile;` impl of the trait.
  - `pub fn collect_fleet_skills(mur_home: &Path, fleet_name: &str) -> anyhow::Result<Vec<(String, SkillManifest)>>` (installed skills with `scope == Fleet && fleet == Some(fleet_name)`).
  - `pub fn cmd_fleet_export(mur_home: &Path, name: &str, with_members: bool, out: Option<PathBuf>, now_rfc3339: &str) -> anyhow::Result<()>`
    (`now_rfc3339` is injected so tests are deterministic; the dispatcher passes `chrono::Utc::now().to_rfc3339()`.)

- [ ] **Step 1: Add tar + flate2 to mur-core/Cargo.toml**

If absent from `[dependencies]` in `mur-core/Cargo.toml`, add (versions matching the workspace lock — `tar = "0.4"`, `flate2 = "1"`):
```toml
tar = "0.4"
flate2 = "1"
```
Run `cargo metadata -q >/dev/null` or a build to confirm resolution.

- [ ] **Step 2: Create the transport seam**

Create `mur-core/src/cmd/fleet/bundle_transport.rs`:

```rust
//! Transport seam for `.fleet` bundles. Phase A ships `LocalFile`; future
//! `TeamServer` / `OfficialRegistry` impls slot in without touching build/parse.

use std::path::Path;

use anyhow::{Context, Result};

/// Reads/writes opaque bundle bytes from/to a location identifier.
pub trait FleetBundleTransport {
    fn read(&self, src: &str) -> Result<Vec<u8>>;
    fn write(&self, dst: &str, bytes: &[u8]) -> Result<()>;
}

/// Local-filesystem transport: `src`/`dst` are file paths.
pub struct LocalFile;

impl FleetBundleTransport for LocalFile {
    fn read(&self, src: &str) -> Result<Vec<u8>> {
        std::fs::read(Path::new(src)).with_context(|| format!("read bundle {src}"))
    }
    fn write(&self, dst: &str, bytes: &[u8]) -> Result<()> {
        std::fs::write(Path::new(dst), bytes).with_context(|| format!("write bundle {dst}"))
    }
}
```

- [ ] **Step 3: Write the failing test for export**

Create `mur-core/src/cmd/fleet/export.rs` with the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::fleet::Fleet;
    use mur_common::fleet_bundle::{BundleManifest, content_hash, verify_manifest_sig};
    use mur_common::identity::AgentIdentity;
    use mur_common::skill::manifest::{SkillManifest, SkillScope};

    fn seed_concierge(home: &std::path::Path) {
        let dir = home.join("agents").join("mur");
        std::fs::create_dir_all(&dir).unwrap();
        AgentIdentity::generate().save(&dir).unwrap();
    }

    fn seed_skill(home: &std::path::Path, name: &str, scope: SkillScope, fleet: Option<&str>) {
        let mut m = SkillManifest {
            name: name.to_string(),
            scope,
            fleet: fleet.map(str::to_string),
            ..test_manifest(name)
        };
        m.scope = scope; // explicit
        let dir = mur_common::skill::store::global_skill_dir(home, name);
        mur_common::skill::store::write_to_dir(&dir, &m).unwrap();
    }

    // Minimal valid manifest builder (fill required fields per SkillManifest).
    fn test_manifest(name: &str) -> SkillManifest {
        serde_yaml::from_str(&format!(
            "name: {name}\nversion: 1.0.0\npublisher: human:t\ndescription: t\n\
             category: context\ncontent:\n  abstract: a\n  context: body\n"
        ))
        .unwrap()
    }

    #[test]
    fn collect_fleet_skills_filters_by_scope_and_fleet() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        seed_skill(home, "in", SkillScope::Fleet, Some("dev"));
        seed_skill(home, "other", SkillScope::Fleet, Some("ops"));
        seed_skill(home, "userk", SkillScope::User, None);
        let got = collect_fleet_skills(home, "dev").unwrap();
        let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["in"]);
    }

    #[test]
    fn export_writes_a_verifiable_signed_bundle() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        seed_concierge(home);
        seed_skill(home, "triage", SkillScope::Fleet, Some("dev"));
        // a minimal fleet
        let fleet = Fleet {
            name: "dev".into(),
            display_name: String::new(),
            goal: "ship".into(),
            router: None,
            members: vec!["pm".into()],
            channel_id: "fleet-dev".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg: None,
        };
        crate::cmd::fleet::store::save_fleet(home, &fleet).unwrap();

        let out = home.join("dev.fleet");
        cmd_fleet_export(home, "dev", false, Some(out.clone()), "2026-06-20T00:00:00Z").unwrap();
        assert!(out.is_file());

        // re-open: unpack, read manifest, verify signature + entry hashes
        let bytes = std::fs::read(&out).unwrap();
        let (manifest, files) = crate::cmd::fleet::import::unpack_for_test(&bytes).unwrap();
        let (_, pk) = multibase::decode(&manifest.signer_pubkey).unwrap();
        let pk: [u8; 32] = pk.try_into().unwrap();
        assert!(verify_manifest_sig(&manifest, &pk));
        assert!(manifest.entries.iter().any(|e| e.path == "fleet.yaml"));
        assert!(
            manifest
                .entries
                .iter()
                .any(|e| e.path == "skills/triage/skill.yaml")
        );
        // every entry hash matches the unpacked file bytes
        for e in &manifest.entries {
            assert_eq!(content_hash(&files[&e.path]), e.sha256);
        }
        assert!(!manifest.includes_members);
        assert_eq!(manifest.members, vec!["pm".to_string()]);
    }
}
```

(Note: `unpack_for_test` is a small `pub(crate)` helper added in Task 3's import.rs; if Task 3 is not yet implemented, temporarily inline an unpack in this test, then switch to `import::unpack_for_test` in Task 3. Implementer: implement Task 2 and Task 3 in sequence; this shared helper is defined in Task 3, Step 2.)

- [ ] **Step 4: Run the test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core -E 'test(/export_writes_a_verifiable/) + test(/collect_fleet_skills/)'`
Expected: FAIL — `collect_fleet_skills` / `cmd_fleet_export` not defined.

- [ ] **Step 5: Implement export.rs**

Prepend to `mur-core/src/cmd/fleet/export.rs`:

```rust
//! `mur fleet export`: package a fleet's definition + its fleet-scoped skills
//! (+ optional member agents — Task 4) into a signed `.fleet` bundle.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use flate2::Compression;
use flate2::write::GzEncoder;
use mur_common::fleet_bundle::{
    BundleEntry, BundleManifest, FLEET_BUNDLE_FORMAT, content_hash, manifest_sign_input,
    signer_fingerprint,
};
use mur_common::identity::AgentIdentity;
use mur_common::skill::manifest::{SkillManifest, SkillScope};

use super::bundle_transport::{FleetBundleTransport, LocalFile};
use super::store;

/// Installed skills whose scope targets exactly this fleet.
pub fn collect_fleet_skills(mur_home: &Path, fleet_name: &str) -> Result<Vec<(String, SkillManifest)>> {
    let mut out = Vec::new();
    for name in mur_common::skill::local::list_installed(mur_home).unwrap_or_default() {
        let Ok(m) = mur_common::skill::local::load_installed(mur_home, &name) else {
            continue;
        };
        if m.scope == SkillScope::Fleet && m.fleet.as_deref() == Some(fleet_name) {
            out.push((name, m));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0)); // deterministic order
    Ok(out)
}

/// Add one in-memory blob to a tar builder at `path`.
fn add_blob<W: Write>(tar: &mut tar::Builder<W>, path: &str, data: &[u8]) -> Result<()> {
    let mut h = tar::Header::new_gnu();
    h.set_size(data.len() as u64);
    h.set_mode(0o644);
    h.set_cksum();
    tar.append_data(&mut h, path, data)
        .with_context(|| format!("tar add {path}"))?;
    Ok(())
}

/// Build the `.fleet` (tar.gz) bytes from a manifest + the (path, bytes) files.
fn build_bundle_bytes(manifest: &BundleManifest, files: &[(String, Vec<u8>)]) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    {
        let gz = GzEncoder::new(&mut buf, Compression::default());
        let mut tar = tar::Builder::new(gz);
        let manifest_yaml = serde_yaml::to_string(manifest).context("serialize manifest")?;
        add_blob(&mut tar, "bundle.yaml", manifest_yaml.as_bytes())?;
        for (path, bytes) in files {
            add_blob(&mut tar, path, bytes)?;
        }
        tar.into_inner().context("finish tar")?.finish().context("flush gzip")?;
    }
    Ok(buf)
}

pub fn cmd_fleet_export(
    mur_home: &Path,
    name: &str,
    with_members: bool,
    out: Option<PathBuf>,
    now_rfc3339: &str,
) -> Result<()> {
    let fleet = store::load_fleet(mur_home, name)?;

    // 1. Collect files: fleet.yaml (host-specific .last_run/.stopped are separate
    //    sentinel files, never in fleet.yaml, so nothing to strip) + scoped skills.
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let fleet_yaml = serde_yaml::to_string(&fleet).context("serialize fleet")?;
    files.push(("fleet.yaml".to_string(), fleet_yaml.into_bytes()));

    for (skill_name, m) in collect_fleet_skills(mur_home, name)? {
        let yaml = serde_yaml::to_string(&m).context("serialize skill")?;
        files.push((format!("skills/{skill_name}/skill.yaml"), yaml.into_bytes()));
    }

    // 2. Members (Task 4 fills this in when with_members=true).
    if with_members {
        super::export::add_member_exports(mur_home, &fleet, &mut files)?;
    }

    // 3. Manifest: pin every file by hash, sign with the concierge identity.
    let id = AgentIdentity::load(&mur_home.join("agents").join("mur"))
        .context("load concierge identity (~/.mur/agents/mur)")?;
    let signer_pubkey = id.public_key_multibase();
    let entries: Vec<BundleEntry> = files
        .iter()
        .map(|(p, b)| BundleEntry { path: p.clone(), sha256: content_hash(b) })
        .collect();
    let mut manifest = BundleManifest {
        format_version: FLEET_BUNDLE_FORMAT,
        fleet_name: name.to_string(),
        created_at: now_rfc3339.to_string(),
        signer_fingerprint: signer_fingerprint(&signer_pubkey),
        signer_pubkey,
        includes_members: with_members,
        members: fleet.members.clone(),
        entries,
        sig: None,
    };
    let input = manifest_sign_input(&manifest);
    manifest.sig = Some(multibase::encode(multibase::Base::Base58Btc, id.sign_bytes(&input)));

    // 4. Build + write via transport.
    let bytes = build_bundle_bytes(&manifest, &files)?;
    let out = out.unwrap_or_else(|| PathBuf::from(format!("{name}.fleet")));
    let out_str = out.to_str().context("output path is not UTF-8")?;
    LocalFile.write(out_str, &bytes)?;

    println!(
        "Exported fleet '{name}' → {} (signer {}, {} skill(s){})",
        out.display(),
        manifest.signer_fingerprint,
        manifest.entries.len().saturating_sub(1),
        if with_members { ", members included" } else { "" }
    );
    Ok(())
}
```

Add a temporary stub so this compiles before Task 4 (Task 4 replaces the body):
```rust
/// Task 4 fills this in. Stub: members not yet bundled.
pub(crate) fn add_member_exports(
    _mur_home: &Path,
    _fleet: &mur_common::fleet::Fleet,
    _files: &mut Vec<(String, Vec<u8>)>,
) -> Result<()> {
    bail!("--with-members not yet implemented");
}
```

Add to `mur-core/src/cmd/fleet/mod.rs`: `pub mod bundle_transport;` and `pub mod export;`.

- [ ] **Step 6: Wire the CLI (Export only)**

In `mur-core/src/cli/actions.rs`, add to `FleetAction` (after `Start`):
```rust
    /// Export a fleet definition + its fleet-scoped skills to a signed .fleet bundle
    Export {
        name: String,
        /// Also bundle the member agents (profile minus signing key + skills)
        #[arg(long)]
        with_members: bool,
        /// Output path (default: <name>.fleet)
        #[arg(short = 'o', long)]
        out: Option<std::path::PathBuf>,
    },
```

In `mur-core/src/dispatch.rs`, in the `Commands::Fleet` match, add:
```rust
        FleetAction::Export { name, with_members, out } => cmd::fleet::export::cmd_fleet_export(
            &mur_home,
            &name,
            with_members,
            out,
            &chrono::Utc::now().to_rfc3339(),
        )?,
```

- [ ] **Step 7: Run the export tests to verify they pass**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core -E 'test(/collect_fleet_skills/) + test(/export_writes_a_verifiable/)'`
Expected: PASS (after Task 3's `unpack_for_test` exists; if running Task 2 standalone, temporarily inline unpack in the test).

- [ ] **Step 8: Lint + commit**

Run: `cargo fmt -p mur-core && ORT_STRATEGY=download cargo clippy -p mur-core --no-deps -- -D warnings`
```bash
git add mur-core/src/cmd/fleet/{bundle_transport.rs,export.rs,mod.rs} mur-core/src/cli/actions.rs mur-core/src/dispatch.rs mur-core/Cargo.toml
git commit -m "feat(fleet): mur fleet export — signed .fleet bundle (definition + scoped skills)"
```

---

## Task 3: `mur fleet import` (core + member missing-report)

**Files:**
- Create: `mur-core/src/cmd/fleet/import.rs`
- Modify: `mur-core/src/cmd/fleet/mod.rs`, `mur-core/src/cli/actions.rs`, `mur-core/src/dispatch.rs`
- Test: inline tests in `import.rs`

**Interfaces:**
- Consumes: Task 1 (`BundleManifest`, `verify_manifest_sig`, `content_hash`), Task 2 (`FleetBundleTransport`, `LocalFile`); `crate::cmd::fleet::store::{save_fleet, fleet_path}`; `mur_common::skill::{store::{global_skill_dir, write_to_dir}, local::set_trust_level, scan::scan_skill, manifest::{SkillManifest, SkillScope}, types::TrustLevel}`; `crate::a2a_dial::canonicalize_agent_name`.
- Produces:
  - `pub struct ImportOpts { pub force: bool, pub no_members: bool, pub yes: bool }`
  - `pub fn cmd_fleet_import(mur_home: &Path, file: &Path, opts: ImportOpts) -> anyhow::Result<()>`
  - `pub(crate) fn unpack_for_test(bytes: &[u8]) -> anyhow::Result<(BundleManifest, std::collections::HashMap<String, Vec<u8>>)>` (shared with Task 2's test)
  - `pub fn missing_members(mur_home: &Path, members: &[String]) -> Vec<String>`

- [ ] **Step 1: Write the failing tests for import**

Create `mur-core/src/cmd/fleet/import.rs` with the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::fleet::Fleet;
    use mur_common::identity::AgentIdentity;
    use mur_common::skill::manifest::SkillScope;
    use mur_common::skill::types::TrustLevel;

    fn export_fixture(home: &std::path::Path) -> std::path::PathBuf {
        // concierge + a fleet-scoped skill + a fleet, then export
        let dir = home.join("agents").join("mur");
        std::fs::create_dir_all(&dir).unwrap();
        AgentIdentity::generate().save(&dir).unwrap();
        let m: mur_common::skill::manifest::SkillManifest = serde_yaml::from_str(
            "name: triage\nversion: 1.0.0\npublisher: human:t\ndescription: t\n\
             category: context\nscope: fleet\nfleet: dev\ncontent:\n  abstract: a\n  context: body\n",
        )
        .unwrap();
        mur_common::skill::store::write_to_dir(
            &mur_common::skill::store::global_skill_dir(home, "triage"),
            &m,
        )
        .unwrap();
        let fleet = Fleet {
            name: "dev".into(), display_name: String::new(), goal: "ship".into(),
            router: None, members: vec!["pm".into(), "qa".into()],
            channel_id: "fleet-dev".into(), rules: vec![], skills: vec![], loop_cfg: None,
        };
        crate::cmd::fleet::store::save_fleet(home, &fleet).unwrap();
        let out = home.join("dev.fleet");
        crate::cmd::fleet::export::cmd_fleet_export(home, "dev", false, Some(out.clone()), "2026-06-20T00:00:00Z").unwrap();
        out
    }

    #[test]
    fn import_roundtrip_installs_fleet_and_skill_at_low_trust() {
        let src = tempfile::tempdir().unwrap();
        let bundle = export_fixture(src.path());

        let dst = tempfile::tempdir().unwrap();
        let home = dst.path();
        cmd_fleet_import(
            home, &bundle,
            ImportOpts { force: false, no_members: false, yes: true },
        )
        .unwrap();

        // fleet installed
        let f = crate::cmd::fleet::store::load_fleet(home, "dev").unwrap();
        assert_eq!(f.members, vec!["pm".to_string(), "qa".to_string()]);
        // skill installed, scope:Fleet preserved, trust downgraded to Sandboxed
        let m = mur_common::skill::local::load_installed(home, "triage").unwrap();
        assert_eq!(m.scope, SkillScope::Fleet);
        assert_eq!(m.fleet.as_deref(), Some("dev"));
        assert_eq!(
            mur_common::skill::local::get_trust_level(home, "triage").unwrap(),
            TrustLevel::Sandboxed
        );
    }

    #[test]
    fn import_refuses_tampered_bundle() {
        let src = tempfile::tempdir().unwrap();
        let bundle = export_fixture(src.path());
        // flip a byte in the archive
        let mut bytes = std::fs::read(&bundle).unwrap();
        let n = bytes.len();
        bytes[n / 2] ^= 0xFF;
        let bad = src.path().join("bad.fleet");
        std::fs::write(&bad, &bytes).unwrap();

        let dst = tempfile::tempdir().unwrap();
        let err = cmd_fleet_import(
            dst.path(), &bad,
            ImportOpts { force: false, no_members: false, yes: true },
        )
        .unwrap_err();
        assert!(format!("{err:#}").to_lowercase().contains("verif") || format!("{err:#}").to_lowercase().contains("hash") || format!("{err:#}").to_lowercase().contains("gzip") || format!("{err:#}").to_lowercase().contains("archive"));
    }

    #[test]
    fn import_refuses_name_conflict_without_force() {
        let src = tempfile::tempdir().unwrap();
        let bundle = export_fixture(src.path());
        let dst = tempfile::tempdir().unwrap();
        let home = dst.path();
        let opts = || ImportOpts { force: false, no_members: false, yes: true };
        cmd_fleet_import(home, &bundle, opts()).unwrap();
        let err = cmd_fleet_import(home, &bundle, opts()).unwrap_err();
        assert!(format!("{err:#}").contains("exists"));
        // with force it succeeds
        cmd_fleet_import(home, &bundle, ImportOpts { force: true, no_members: false, yes: true }).unwrap();
    }

    #[test]
    fn missing_members_reports_absent_agents() {
        let dst = tempfile::tempdir().unwrap();
        let home = dst.path();
        // create agent "pm" locally
        let pm = home.join("agents").join("pm");
        std::fs::create_dir_all(&pm).unwrap();
        std::fs::write(pm.join("profile.yaml"), "name: pm\n").unwrap();
        let missing = missing_members(home, &["pm".into(), "qa".into()]);
        assert_eq!(missing, vec!["qa".to_string()]);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core -E 'test(/import_roundtrip/) + test(/import_refuses/) + test(/missing_members/)'`
Expected: FAIL — `cmd_fleet_import` / `ImportOpts` / `missing_members` not defined.

- [ ] **Step 3: Implement import.rs**

Prepend to `mur-core/src/cmd/fleet/import.rs`:

```rust
//! `mur fleet import`: verify a `.fleet` bundle (untrusted observed data),
//! security-scan its skills, confirm, then install. Never auto-runs the fleet.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use mur_common::fleet::Fleet;
use mur_common::fleet_bundle::{BundleManifest, content_hash, verify_manifest_sig};
use mur_common::skill::manifest::{SkillManifest, SkillScope};
use mur_common::skill::types::TrustLevel;

use super::bundle_transport::{FleetBundleTransport, LocalFile};
use super::store;

pub struct ImportOpts {
    pub force: bool,
    pub no_members: bool,
    pub yes: bool,
}

/// Unpack the tar.gz into (manifest, path->bytes). Rejects unsafe entry paths.
pub(crate) fn unpack_for_test(bytes: &[u8]) -> Result<(BundleManifest, HashMap<String, Vec<u8>>)> {
    let gz = GzDecoder::new(bytes);
    let mut ar = tar::Archive::new(gz);
    let mut files: HashMap<String, Vec<u8>> = HashMap::new();
    for entry in ar.entries().context("read archive")? {
        let mut entry = entry.context("archive entry")?;
        let path = entry
            .path()
            .context("entry path")?
            .to_string_lossy()
            .to_string();
        // Path-traversal guard: relative, no `..`, no absolute.
        if path.starts_with('/') || path.split('/').any(|c| c == "..") {
            bail!("unsafe bundle entry path: {path}");
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).context("read entry")?;
        files.insert(path, buf);
    }
    let manifest_bytes = files
        .get("bundle.yaml")
        .context("bundle.yaml missing from archive")?;
    let manifest: BundleManifest =
        serde_yaml::from_slice(manifest_bytes).context("parse bundle.yaml")?;
    Ok((manifest, files))
}

/// Member names with no local agent (`agents/<name>/profile.yaml` absent).
pub fn missing_members(mur_home: &Path, members: &[String]) -> Vec<String> {
    members
        .iter()
        .filter(|m| {
            let canon = crate::a2a_dial::canonicalize_agent_name(mur_home, m);
            !mur_home
                .join("agents")
                .join(&canon)
                .join("profile.yaml")
                .is_file()
        })
        .cloned()
        .collect()
}

/// Prompt y/N unless `yes`. Returns true to proceed.
fn confirm(prompt: &str, yes: bool) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    use std::io::Write;
    print!("{prompt} [y/N] ");
    std::io::stdout().flush().ok();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).context("read stdin")?;
    Ok(matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

pub fn cmd_fleet_import(mur_home: &Path, file: &Path, opts: ImportOpts) -> Result<()> {
    // 1. Read + unpack (transport seam).
    let src = file.to_str().context("bundle path is not UTF-8")?;
    let bytes = LocalFile.read(src)?;
    let (manifest, files) = unpack_for_test(&bytes)?;

    // 2. Verify signature (fail-closed). Unsigned → refuse unless --force.
    let (_, pk) = multibase::decode(&manifest.signer_pubkey).context("decode signer pubkey")?;
    let pk: [u8; 32] = pk
        .try_into()
        .map_err(|_| anyhow::anyhow!("signer pubkey is not 32 bytes"))?;
    if manifest.sig.is_none() {
        if !opts.force {
            bail!("bundle is UNSIGNED; re-run with --force to import as untrusted");
        }
    } else if !verify_manifest_sig(&manifest, &pk) {
        bail!("bundle signature verification FAILED — refusing import");
    }

    // 3. Verify every entry's hash against the unpacked bytes (fail-closed).
    for e in &manifest.entries {
        let got = files
            .get(&e.path)
            .with_context(|| format!("bundle missing entry {}", e.path))?;
        if content_hash(got) != e.sha256 {
            bail!("hash mismatch for {} — refusing import", e.path);
        }
    }

    // 4. Provenance + plan (two-tier trust: Phase A pins an empty official set, so
    //    every bundle is a peer/TOFU import → lowest trust, scan + confirm).
    let skill_paths: Vec<&String> = manifest
        .entries
        .iter()
        .map(|e| &e.path)
        .filter(|p| p.starts_with("skills/"))
        .collect();
    println!("Fleet bundle '{}' from signer {}", manifest.fleet_name, manifest.signer_fingerprint);
    println!("  signature: {}", if manifest.sig.is_some() { "verified" } else { "UNSIGNED (--force)" });
    println!("  skills: {}", skill_paths.len());
    println!("  members declared: {}", manifest.members.join(", "));

    // 5. Security-scan each bundled skill; surface findings.
    let mut parsed_skills: Vec<(String, SkillManifest)> = Vec::new();
    for path in &skill_paths {
        let m: SkillManifest = serde_yaml::from_slice(&files[*path])
            .with_context(|| format!("parse {path}"))?;
        let report = mur_common::skill::scan::scan_skill(&m)
            .map_err(|e| anyhow::anyhow!("scan {path}: {e}"))?;
        if report.has_blocking_findings() {
            println!("  ⚠ security findings in {}:", m.name);
            for line in report.human_summary() {
                println!("      {line}");
            }
        }
        parsed_skills.push((m.name.clone(), m));
    }

    // 6. HITL confirm — nothing written before approval.
    if !confirm("Install this fleet + skills?", opts.yes)? {
        bail!("import cancelled");
    }

    // 7. Conflict checks (fail-closed unless --force).
    if store::fleet_path(mur_home, &manifest.fleet_name).is_file() && !opts.force {
        bail!("fleet '{}' already exists — re-run with --force to overwrite", manifest.fleet_name);
    }

    // 8. Install skills: scope:Fleet preserved, trust DOWNGRADED to Sandboxed.
    for (name, mut m) in parsed_skills {
        let dir = mur_common::skill::store::global_skill_dir(mur_home, &name);
        if dir.join("skill.yaml").is_file() && !opts.force {
            println!("  skill '{name}' exists — skipping (use --force to overwrite)");
            continue;
        }
        // enforce scope:Fleet for this fleet (provenance ≠ claim)
        m.scope = SkillScope::Fleet;
        m.fleet = Some(manifest.fleet_name.clone());
        m.project = None;
        mur_common::skill::store::write_to_dir(&dir, &m)
            .map_err(|e| anyhow::anyhow!("install skill {name}: {e}"))?;
        mur_common::skill::local::set_trust_level(mur_home, &name, TrustLevel::Sandboxed)
            .map_err(|e| anyhow::anyhow!("set trust {name}: {e}"))?;
    }

    // 9. Install the fleet definition.
    let fleet: Fleet = serde_yaml::from_slice(
        files.get("fleet.yaml").context("bundle missing fleet.yaml")?,
    )
    .context("parse fleet.yaml")?;
    store::save_fleet(mur_home, &fleet)?;

    // 10. Members: install bundled (Task 4) or report missing. Never auto-run.
    if manifest.includes_members && !opts.no_members {
        super::import::install_bundled_members(mur_home, &manifest, &files, opts.force, opts.yes)?;
    }
    let missing = missing_members(mur_home, &fleet.members);
    if missing.is_empty() {
        println!("Imported fleet '{}'. All members present.", fleet.name);
    } else {
        println!(
            "Imported fleet '{}'. Missing members: {} — create them or import a --with-members bundle before running.",
            fleet.name,
            missing.join(", ")
        );
    }
    Ok(())
}
```

Add a temporary stub (Task 4 replaces it):
```rust
/// Task 4 fills this in. Stub: bundled-member install not yet implemented.
pub(crate) fn install_bundled_members(
    _mur_home: &Path,
    _manifest: &BundleManifest,
    _files: &HashMap<String, Vec<u8>>,
    _force: bool,
    _yes: bool,
) -> Result<()> {
    bail!("bundled-member install not yet implemented");
}
```

Add `pub mod import;` to `mur-core/src/cmd/fleet/mod.rs`.

- [ ] **Step 4: Wire the CLI (Import)**

In `mur-core/src/cli/actions.rs`, add to `FleetAction`:
```rust
    /// Import a fleet from a .fleet bundle (verifies signature, scans skills, confirms)
    Import {
        /// Path to the .fleet bundle
        file: std::path::PathBuf,
        /// Overwrite an existing fleet/skill of the same name
        #[arg(long)]
        force: bool,
        /// Skip member-agent install even if the bundle includes them
        #[arg(long)]
        no_members: bool,
        /// Pre-approve the install confirmation (still verifies + scans)
        #[arg(long)]
        yes: bool,
    },
```

In `mur-core/src/dispatch.rs`, add the arm:
```rust
        FleetAction::Import { file, force, no_members, yes } => cmd::fleet::import::cmd_fleet_import(
            &mur_home,
            &file,
            cmd::fleet::import::ImportOpts { force, no_members, yes },
        )?,
```

- [ ] **Step 5: Run the import tests + the Task-2 export test to verify they pass**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core -E 'test(/import_/) + test(/missing_members/) + test(/export_writes_a_verifiable/) + test(/collect_fleet_skills/)'`
Expected: PASS (all).

- [ ] **Step 6: Lint + commit**

Run: `cargo fmt -p mur-core && ORT_STRATEGY=download cargo clippy -p mur-core --no-deps -- -D warnings`
```bash
git add mur-core/src/cmd/fleet/{import.rs,mod.rs} mur-core/src/cli/actions.rs mur-core/src/dispatch.rs
git commit -m "feat(fleet): mur fleet import — verify + scan + HITL + install (peer TOFU, low trust)"
```

---

## Task 4: `--with-members` (turnkey, least-privilege)

**Files:**
- Modify: `mur-core/src/cmd/fleet/export.rs` (`add_member_exports`), `mur-core/src/cmd/fleet/import.rs` (`install_bundled_members`)
- Test: extend inline tests in both files

**Interfaces:**
- Consumes: existing agent profile layout (`agents/<name>/profile.yaml`); `mur_common::identity::AgentIdentity::{generate, save}`; `crate::a2a_dial::canonicalize_agent_name`.
- Produces (replaces Task 2/3 stubs): `add_member_exports(mur_home, fleet, files)`; `install_bundled_members(mur_home, manifest, files, force, yes)`.

**Member packaging rule (export):** for each member with a local `agents/<name>/profile.yaml`, add `members/<name>/profile.yaml` (the profile **as-is** — it never contains the private key; `identity.key` is a separate file and is **not** read, matching `fleet_sync::build_fleet_profile_changes`). Phase A bundles the profile only (skills travel as the fleet's scoped skills); per-member private skills are out of scope (documented).

**Member install rule (import):** for each member in the bundle that is **missing** locally: show its profile's `entitlements`, confirm, then create `agents/<name>/`, write `profile.yaml`, and **generate a fresh local identity** (`AgentIdentity::generate().save(dir)`). **Never overwrite** an existing local agent (skip + report unless `--force`). Secrets never travel (only `profile.yaml`, no `identity.key`, no secret store).

- [ ] **Step 1: Write the failing tests**

Add to `export.rs` tests:
```rust
    #[test]
    fn export_with_members_bundles_profile_without_private_key() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        seed_concierge(home);
        // member agent "pm" with a profile + a private key
        let pm = home.join("agents").join("pm");
        std::fs::create_dir_all(&pm).unwrap();
        std::fs::write(pm.join("profile.yaml"), "name: pm\nentitlements: {}\n").unwrap();
        AgentIdentity::generate().save(&pm).unwrap(); // writes identity.key
        let fleet = Fleet {
            name: "dev".into(), display_name: String::new(), goal: "g".into(), router: None,
            members: vec!["pm".into()], channel_id: "fleet-dev".into(),
            rules: vec![], skills: vec![], loop_cfg: None,
        };
        crate::cmd::fleet::store::save_fleet(home, &fleet).unwrap();
        let out = home.join("dev.fleet");
        cmd_fleet_export(home, "dev", true, Some(out.clone()), "2026-06-20T00:00:00Z").unwrap();

        let bytes = std::fs::read(&out).unwrap();
        let (manifest, files) = crate::cmd::fleet::import::unpack_for_test(&bytes).unwrap();
        assert!(manifest.includes_members);
        assert!(files.contains_key("members/pm/profile.yaml"));
        // NO private key travels
        assert!(!files.keys().any(|k| k.contains("identity.key")));
    }
```

Add to `import.rs` tests:
```rust
    #[test]
    fn import_with_members_creates_member_with_fresh_identity() {
        // export a --with-members bundle from a source home
        let src = tempfile::tempdir().unwrap();
        let s = src.path();
        let sdir = s.join("agents").join("mur");
        std::fs::create_dir_all(&sdir).unwrap();
        AgentIdentity::generate().save(&sdir).unwrap();
        let pm = s.join("agents").join("pm");
        std::fs::create_dir_all(&pm).unwrap();
        std::fs::write(pm.join("profile.yaml"), "name: pm\nentitlements: {}\n").unwrap();
        AgentIdentity::generate().save(&pm).unwrap();
        let fleet = Fleet {
            name: "dev".into(), display_name: String::new(), goal: "g".into(), router: None,
            members: vec!["pm".into()], channel_id: "fleet-dev".into(),
            rules: vec![], skills: vec![], loop_cfg: None,
        };
        crate::cmd::fleet::store::save_fleet(s, &fleet).unwrap();
        let bundle = s.join("dev.fleet");
        crate::cmd::fleet::export::cmd_fleet_export(s, "dev", true, Some(bundle.clone()), "2026-06-20T00:00:00Z").unwrap();

        // import into a fresh home
        let dst = tempfile::tempdir().unwrap();
        let home = dst.path();
        cmd_fleet_import(home, &bundle, ImportOpts { force: false, no_members: false, yes: true }).unwrap();

        let pm2 = home.join("agents").join("pm");
        assert!(pm2.join("profile.yaml").is_file());
        assert!(pm2.join("identity.key").is_file()); // fresh identity generated locally
        assert!(missing_members(home, &["pm".into()]).is_empty());
    }

    #[test]
    fn import_with_members_never_overwrites_existing_agent() {
        let src = tempfile::tempdir().unwrap();
        let s = src.path();
        let sdir = s.join("agents").join("mur");
        std::fs::create_dir_all(&sdir).unwrap();
        AgentIdentity::generate().save(&sdir).unwrap();
        let pm = s.join("agents").join("pm");
        std::fs::create_dir_all(&pm).unwrap();
        std::fs::write(pm.join("profile.yaml"), "name: pm\nentitlements: {}\n").unwrap();
        AgentIdentity::generate().save(&pm).unwrap();
        let fleet = Fleet {
            name: "dev".into(), display_name: String::new(), goal: "g".into(), router: None,
            members: vec!["pm".into()], channel_id: "fleet-dev".into(),
            rules: vec![], skills: vec![], loop_cfg: None,
        };
        crate::cmd::fleet::store::save_fleet(s, &fleet).unwrap();
        let bundle = s.join("dev.fleet");
        crate::cmd::fleet::export::cmd_fleet_export(s, "dev", true, Some(bundle.clone()), "2026-06-20T00:00:00Z").unwrap();

        // dest already has a "pm" agent with a sentinel profile
        let dst = tempfile::tempdir().unwrap();
        let home = dst.path();
        let existing = home.join("agents").join("pm");
        std::fs::create_dir_all(&existing).unwrap();
        std::fs::write(existing.join("profile.yaml"), "name: pm\nmine: true\n").unwrap();

        cmd_fleet_import(home, &bundle, ImportOpts { force: false, no_members: false, yes: true }).unwrap();
        // existing profile untouched
        let body = std::fs::read_to_string(existing.join("profile.yaml")).unwrap();
        assert!(body.contains("mine: true"));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core -E 'test(/with_members/)'`
Expected: FAIL — current stubs `bail!`.

- [ ] **Step 3: Implement `add_member_exports` (export.rs)**

Replace the stub in `export.rs`:
```rust
/// Bundle each member's `profile.yaml` (never the private `identity.key`, matching
/// fleet_sync's profile-only assembly) under `members/<name>/`.
pub(crate) fn add_member_exports(
    mur_home: &Path,
    fleet: &mur_common::fleet::Fleet,
    files: &mut Vec<(String, Vec<u8>)>,
) -> Result<()> {
    for member in &fleet.members {
        let canon = crate::a2a_dial::canonicalize_agent_name(mur_home, member);
        let profile = mur_home.join("agents").join(&canon).join("profile.yaml");
        if !profile.is_file() {
            // skip absent members; import side reports them missing
            continue;
        }
        let body = std::fs::read(&profile).with_context(|| format!("read {}", profile.display()))?;
        files.push((format!("members/{canon}/profile.yaml"), body));
    }
    Ok(())
}
```

- [ ] **Step 4: Implement `install_bundled_members` (import.rs)**

Replace the stub in `import.rs`:
```rust
/// Install each bundled member that is MISSING locally: show entitlements, confirm,
/// write profile.yaml, and generate a FRESH local identity (private keys never
/// travel). Never overwrites an existing local agent.
pub(crate) fn install_bundled_members(
    mur_home: &Path,
    manifest: &BundleManifest,
    files: &HashMap<String, Vec<u8>>,
    force: bool,
    yes: bool,
) -> Result<()> {
    for member in &manifest.members {
        let canon = crate::a2a_dial::canonicalize_agent_name(mur_home, member);
        let dir = mur_home.join("agents").join(&canon);
        let key = format!("members/{canon}/profile.yaml");
        let Some(profile_bytes) = files.get(&key) else {
            continue; // not bundled; will be reported missing
        };
        if dir.join("profile.yaml").is_file() && !force {
            println!("  member '{canon}' already exists — skipping (use --force to overwrite)");
            continue;
        }
        // surface entitlements from the profile for explicit approval
        let profile_str = String::from_utf8_lossy(profile_bytes);
        let ent_line = profile_str
            .lines()
            .find(|l| l.trim_start().starts_with("entitlements"))
            .unwrap_or("entitlements: (none declared)");
        if !confirm(
            &format!("Install member agent '{canon}'? ({})", ent_line.trim()),
            yes,
        )? {
            println!("  member '{canon}' skipped by user");
            continue;
        }
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        std::fs::write(dir.join("profile.yaml"), profile_bytes)
            .with_context(|| format!("write profile for {canon}"))?;
        // fresh local identity — exporter's private key never travels
        mur_common::identity::AgentIdentity::generate()
            .save(&dir)
            .with_context(|| format!("generate identity for {canon}"))?;
        println!("  installed member '{canon}' (new local identity)");
    }
    Ok(())
}
```

- [ ] **Step 5: Run all fleet bundle tests to verify they pass**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-core -E 'test(/with_members/) + test(/import_/) + test(/export_/) + test(/collect_fleet_skills/) + test(/missing_members/)'`
Expected: PASS (all).

- [ ] **Step 6: Update CLAUDE.md (one line) + lint + commit**

In `CLAUDE.md`, in the `mur fleet {…}` surface line, add `export`/`import` to the subcommand list and append a sentence:
> `export <name> [--with-members]` / `import <file> [--force] [--no-members] [--yes]` — share a fleet via a signed `.fleet` bundle (definition + fleet-scoped skills + optional member agents); import verifies the signature, scans skills, installs at lowest trust (peer TOFU), regenerates member identities locally, and never auto-runs. Local-first; transport seam left for team/official sync.

Run: `cargo fmt -p mur-core && ORT_STRATEGY=download cargo clippy -p mur-core --no-deps -- -D warnings`
```bash
git add mur-core/src/cmd/fleet/{export.rs,import.rs} CLAUDE.md
git commit -m "feat(fleet): --with-members bundle/install (fresh identity, least-privilege, no overwrite)"
```

---

## Self-Review (run by the author before handoff)

**1. Spec coverage** (each spec §):
- §3 components → T1 (`fleet_bundle`), T2 (`bundle_transport`, `export`), T3 (`import`). ✓
- §4 bundle format / sign manifest not archive → T1 `manifest_sign_input`, T2 `build_bundle_bytes`. ✓
- §5 CLI surface → T2 (Export), T3 (Import). ✓
- §6 export flow → T2. ✓ §7 signing identity (concierge `mur` key) → T2 Step 5. ✓
- §8 two-tier trust (Phase A: empty official pin set → peer TOFU, low trust) → T3 Step 4/8. ✓
- §9 import flow (verify, hash, scan, HITL, install, members) → T3. ✓
- §10 members hybrid + least-privilege → T3 (report) + T4 (with-members). ✓
- §11 conflict/re-import (refuse unless --force) → T3 Step 7/8. ✓
- §13 error handling (refuse on mismatch, unsigned, path-traversal) → T1/T3. ✓
- §14 testing (serde, sign/verify/tamper, roundtrip, security, with-members) → T1–T4 tests. ✓
- §12 seams: transport (T2), entitlement seam → **GAP**: the spec mentions a single entitlement check point; Phase A's LocalFile path is ungated (local export is free), so no entitlement fn is needed yet. Acceptable — documented in spec §12 as ungated for LocalFile. No task needed.

**2. Placeholder scan:** Task 2/3 carry deliberate `bail!` STUBS for `add_member_exports`/`install_bundled_members`, each replaced by code in Task 4 (noted at each stub). No vague "handle errors" — all error paths are concrete `bail!`/`Context`. ✓

**3. Type consistency:** `BundleManifest`/`BundleEntry`/`content_hash`/`manifest_sign_input`/`verify_manifest_sig`/`signer_fingerprint` defined in T1, used identically in T2/T3. `ImportOpts{force,no_members,yes}` defined T3, used T3/T4 tests. `cmd_fleet_export(mur_home,name,with_members,out,now_rfc3339)` consistent T2↔T3 tests. `unpack_for_test` defined T3 Step 3, used by T2 test + T4 tests (noted in T2 Step 3). `install_bundled_members`/`add_member_exports` signatures match stub↔impl. `TrustLevel::Sandboxed`, `SkillScope::Fleet` per scout. ✓

**Note for the implementer:** Tasks 2 and 3 share `unpack_for_test` (defined in T3). Implement T2 then T3 in sequence; when running T2's export test standalone before T3 exists, temporarily inline a 6-line unpack in that one test, then delete it once T3 lands.
