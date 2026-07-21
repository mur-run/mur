# Official Catalog — MUR Client Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Client side of the official agents/fleets catalog — license types + verification, official-distribution markers on both bundle formats, import gates that refuse shared official bundles, and `mur official list|install` against the app.mur.run catalog API.

**Architecture:** A pure `OfficialLicense` DSSE-style signed record in `mur-common` (mirrors `fleet_bundle.rs`: serde_json-with-sig-cleared sign input, Ed25519). Both bundle manifests gain an optional `distribution` field inside the signed payload (backward-compatible via `skip_serializing_if`). Import paths gate on the marker: marker ⇒ bundle signer must be the pinned official key AND a locally stored license (signed by the official key, bound to this item and the logged-in user) must exist. `mur official install` downloads bundle+license from the server, verifies fail-closed, stores the license, then dispatches to the existing import/install functions.

**Tech Stack:** Rust (edition 2024), ed25519-dalek, serde/serde_json/serde_yaml_ng, reqwest, wiremock (dev), existing `crate::auth` device-flow tokens.

**Spec:** `docs/superpowers/specs/2026-07-20-official-catalog-design.md`
**Sibling plans (separate, later):** mur-server catalog/download/license endpoints (Go); `mur-run/official-catalog` repo + CI signing.

## Global Constraints

- Rust edition 2024; single source file ≤ 800 lines.
- No hardcoded values: server base URL only via `crate::auth::server_url()`; official key fp only via `mur_common::skill::publisher_trust::MUR_OFFICIAL_PUBLISHER_KEY_FP` (value `"ed25519-861d2acb"`).
- All verification fail-closed: any parse/signature/identity failure refuses install with a clear message.
- License expiry gates **downloads only** — never checked at import of already-licensed items, never at runtime.
- User-facing strings write the brand as **MUR** and point users to **app.mur.run**.
- Test runner: `cargo nextest run -p <crate> <filter>` (plain `cargo test --workspace` is flaky in this repo). Build env: `ORT_STRATEGY=download`, `MUR_WEB_DIST=$HOME/Projects/mur-web/dist` exported.
- fmt/clippy must stay green: `cargo fmt --all && cargo clippy --workspace -- -D warnings`.

---

### Task 1: `OfficialLicense` type + verification (mur-common)

**Files:**
- Create: `mur-common/src/official.rs`
- Modify: `mur-common/src/lib.rs` (add `pub mod official;`)
- Modify: `mur-common/src/muragent/dsse.rs:122` (make `keyid_from_pubkey` `pub`)
- Test: inline `#[cfg(test)]` in `mur-common/src/official.rs`

**Interfaces:**
- Consumes: `mur_common::skill::publisher_trust::MUR_OFFICIAL_PUBLISHER_KEY_FP` (existing `pub const &str`), `ed25519_dalek::{SigningKey, Signature, Signer, VerifyingKey, Verifier}` (already deps).
- Produces (later tasks rely on these exact names):
  - `pub const DISTRIBUTION_OFFICIAL: &str = "official";`
  - `pub struct OfficialLicense { pub format_version: u32, pub user_id: String, pub item: String, pub version: String, pub expires_at: String, pub signer_pubkey: String, pub sig: Option<String> }`
  - `pub fn license_sign_input(l: &OfficialLicense) -> Vec<u8>`
  - `pub fn sign_license(l: &mut OfficialLicense, key: &ed25519_dalek::SigningKey)`
  - `pub fn verify_license_sig(l: &OfficialLicense) -> bool`
  - `pub fn license_key_fp(l: &OfficialLicense) -> Option<String>`
  - `pub enum LicenseCheck { Ok, BadSignature, NotOfficialKey, WrongUser, WrongItem }`
  - `pub fn check_license(l: &OfficialLicense, expected_item: &str, user_id: &str, official_fp: &str) -> LicenseCheck`
  - `pub fn is_official_key_fp(fp: &str) -> bool`
  - `mur_common::muragent::dsse::keyid_from_pubkey(pubkey: &[u8; 32]) -> String` (now `pub`)

- [ ] **Step 1: Make `keyid_from_pubkey` public**

In `mur-common/src/muragent/dsse.rs` change line 122:

```rust
/// Derive keyid from the first 8 hex chars of SHA-256(pubkey).
pub fn keyid_from_pubkey(pubkey: &[u8; 32]) -> String {
```

- [ ] **Step 2: Write failing tests**

Create `mur-common/src/official.rs` with the module doc, then the test module first (types referenced don't exist yet → compile fail is the "failing test"):

```rust
//! Official catalog license — pure types + signing/verification (no I/O).
//!
//! Model mirrors `fleet_bundle.rs`: canonical sign input = the struct
//! serialized as JSON with `sig` cleared; Ed25519 over those bytes. The
//! license binds an official catalog item to one app.mur.run account.
//! Expiry gates downloads/updates only — never installed content.

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn test_license(key: &SigningKey) -> OfficialLicense {
        let mut l = OfficialLicense {
            format_version: OFFICIAL_LICENSE_FORMAT,
            user_id: "user-123".into(),
            item: "fleets/deep-research".into(),
            version: "1.0.0".into(),
            expires_at: "2027-01-01T00:00:00Z".into(),
            signer_pubkey: String::new(),
            sig: None,
        };
        sign_license(&mut l, key);
        l
    }

    #[test]
    fn sign_verify_roundtrip() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let l = test_license(&key);
        assert!(verify_license_sig(&l));
    }

    #[test]
    fn tampered_field_fails_verify() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let mut l = test_license(&key);
        l.user_id = "someone-else".into();
        assert!(!verify_license_sig(&l));
    }

    #[test]
    fn unsigned_fails_verify() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let mut l = test_license(&key);
        l.sig = None;
        assert!(!verify_license_sig(&l));
    }

    #[test]
    fn check_license_matrix() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let l = test_license(&key);
        let fp = license_key_fp(&l).unwrap();
        assert_eq!(
            check_license(&l, "fleets/deep-research", "user-123", &fp),
            LicenseCheck::Ok
        );
        assert_eq!(
            check_license(&l, "fleets/deep-research", "user-999", &fp),
            LicenseCheck::WrongUser
        );
        assert_eq!(
            check_license(&l, "fleets/other", "user-123", &fp),
            LicenseCheck::WrongItem
        );
        assert_eq!(
            check_license(&l, "fleets/deep-research", "user-123", "ed25519-ffffffff"),
            LicenseCheck::NotOfficialKey
        );
        let mut bad = l.clone();
        bad.sig = Some("mtampered".into());
        assert_eq!(
            check_license(&bad, "fleets/deep-research", "user-123", &fp),
            LicenseCheck::BadSignature
        );
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo nextest run -p mur-common official 2>&1 | tail -5`
Expected: compile error (types not defined).

- [ ] **Step 4: Implement**

Above the test module in `mur-common/src/official.rs`:

```rust
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::muragent::dsse::keyid_from_pubkey;
use crate::skill::publisher_trust::MUR_OFFICIAL_PUBLISHER_KEY_FP;

/// License wire-format version. Bump on any breaking change.
pub const OFFICIAL_LICENSE_FORMAT: u32 = 1;

/// Value of the `distribution` marker stamped inside official bundle manifests.
pub const DISTRIBUTION_OFFICIAL: &str = "official";

/// A signed record binding an official catalog item to one app.mur.run account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfficialLicense {
    pub format_version: u32,
    /// app.mur.run account id the license is bound to.
    pub user_id: String,
    /// Catalog item id, e.g. `agents/researcher` or `fleets/deep-research`.
    pub item: String,
    /// Item version this license was issued for.
    pub version: String,
    /// RFC3339 expiry (subscription end + grace). Gates downloads only.
    pub expires_at: String,
    /// Signer's Ed25519 public key, base64 (32 bytes).
    pub signer_pubkey: String,
    /// Base64 Ed25519 signature over `license_sign_input`. `None` = unsigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig: Option<String>,
}

/// Canonical signing input: the license serialized with `sig` cleared.
pub fn license_sign_input(l: &OfficialLicense) -> Vec<u8> {
    let mut unsigned = l.clone();
    unsigned.sig = None;
    serde_json::to_vec(&unsigned).expect("license serializes")
}

/// Sign in place: fills `signer_pubkey` from `key` and sets `sig`.
pub fn sign_license(l: &mut OfficialLicense, key: &SigningKey) {
    use base64::{Engine, engine::general_purpose::STANDARD as B64};
    l.signer_pubkey = B64.encode(key.verifying_key().as_bytes());
    l.sig = None;
    let sig: Signature = key.sign(&license_sign_input(l));
    l.sig = Some(B64.encode(sig.to_bytes()));
}

/// Verify `sig` against the embedded `signer_pubkey`. False on any failure.
pub fn verify_license_sig(l: &OfficialLicense) -> bool {
    use base64::{Engine, engine::general_purpose::STANDARD as B64};
    let Some(sig_b64) = &l.sig else { return false };
    let Ok(pk_bytes) = B64.decode(&l.signer_pubkey) else {
        return false;
    };
    let Ok(pk_arr) = <[u8; 32]>::try_from(pk_bytes.as_slice()) else {
        return false;
    };
    let Ok(vk) = VerifyingKey::from_bytes(&pk_arr) else {
        return false;
    };
    let Ok(sig_bytes) = B64.decode(sig_b64) else {
        return false;
    };
    let Ok(sig_arr) = <[u8; 64]>::try_from(sig_bytes.as_slice()) else {
        return false;
    };
    vk.verify(&license_sign_input(l), &Signature::from_bytes(&sig_arr))
        .is_ok()
}

/// Publisher-trust-style fingerprint (`ed25519-<8hex>`) of the embedded key.
pub fn license_key_fp(l: &OfficialLicense) -> Option<String> {
    use base64::{Engine, engine::general_purpose::STANDARD as B64};
    let pk = B64.decode(&l.signer_pubkey).ok()?;
    let arr = <[u8; 32]>::try_from(pk.as_slice()).ok()?;
    Some(keyid_from_pubkey(&arr))
}

/// Whether `fp` is the pinned MUR-official publisher key fingerprint.
pub fn is_official_key_fp(fp: &str) -> bool {
    fp == MUR_OFFICIAL_PUBLISHER_KEY_FP
}

/// Outcome of a full license check. Order of checks: signature → signer
/// identity → item binding → user binding (fail-closed at the first miss).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LicenseCheck {
    Ok,
    BadSignature,
    NotOfficialKey,
    WrongUser,
    WrongItem,
}

/// Full check against an expected item + logged-in user + official key fp.
/// `official_fp` is a parameter for testability; production callers pass
/// `MUR_OFFICIAL_PUBLISHER_KEY_FP`. Expiry is deliberately NOT checked here.
pub fn check_license(
    l: &OfficialLicense,
    expected_item: &str,
    user_id: &str,
    official_fp: &str,
) -> LicenseCheck {
    if !verify_license_sig(l) {
        return LicenseCheck::BadSignature;
    }
    match license_key_fp(l) {
        Some(fp) if fp == official_fp => {}
        _ => return LicenseCheck::NotOfficialKey,
    }
    if l.item != expected_item {
        return LicenseCheck::WrongItem;
    }
    if l.user_id != user_id {
        return LicenseCheck::WrongUser;
    }
    LicenseCheck::Ok
}
```

Add to `mur-common/src/lib.rs` next to the other module declarations:

```rust
pub mod official;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo nextest run -p mur-common official 2>&1 | tail -5`
Expected: 4 tests PASS. Also: `cargo nextest run -p mur-common dsse 2>&1 | tail -3` still green.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add mur-common/src/official.rs mur-common/src/lib.rs mur-common/src/muragent/dsse.rs
git commit -m "feat(official): OfficialLicense signed record + verification"
```

---

### Task 2: `distribution` marker on fleet `BundleManifest`

**Files:**
- Modify: `mur-common/src/fleet_bundle.rs` (struct `BundleManifest`, line 24)
- Test: inline tests in `mur-common/src/fleet_bundle.rs`

**Interfaces:**
- Produces: `BundleManifest.distribution: Option<String>` — `Some("official")` (compare against `mur_common::official::DISTRIBUTION_OFFICIAL`) marks official bundles. Field sits inside the signed payload; absent field serializes to nothing so **existing signed bundles keep verifying**.

- [ ] **Step 1: Write failing tests**

Append to the existing `#[cfg(test)] mod tests` in `mur-common/src/fleet_bundle.rs` (reuse the file's existing helpers for building a signed manifest — there are signing tests already; follow their pattern for key setup):

```rust
#[test]
fn manifest_without_distribution_keeps_legacy_sign_input() {
    // A manifest with distribution=None must produce byte-identical sign
    // input to one that predates the field (skip_serializing_if).
    let m = BundleManifest {
        format_version: FLEET_BUNDLE_FORMAT,
        fleet_name: "f".into(),
        created_at: "2026-01-01T00:00:00Z".into(),
        signer_pubkey: "z".into(),
        signer_fingerprint: "aa11-bb22".into(),
        includes_members: false,
        members: vec![],
        entries: vec![],
        sig: None,
        distribution: None,
    };
    let json = String::from_utf8(manifest_sign_input(&m)).unwrap();
    assert!(!json.contains("distribution"));
}

#[test]
fn stripping_distribution_breaks_signature() {
    use ed25519_dalek::{Signer, SigningKey};
    let key = SigningKey::from_bytes(&[9u8; 32]);
    let mut m = BundleManifest {
        format_version: FLEET_BUNDLE_FORMAT,
        fleet_name: "f".into(),
        created_at: "2026-01-01T00:00:00Z".into(),
        signer_pubkey: multibase::encode(
            multibase::Base::Base58Btc,
            key.verifying_key().as_bytes(),
        ),
        signer_fingerprint: "aa11-bb22".into(),
        includes_members: false,
        members: vec![],
        entries: vec![],
        sig: None,
        distribution: Some(crate::official::DISTRIBUTION_OFFICIAL.into()),
    };
    let sig = key.sign(&manifest_sign_input(&m));
    m.sig = Some(multibase::encode(multibase::Base::Base58Btc, sig.to_bytes()));
    let pk = key.verifying_key().to_bytes();
    assert!(verify_manifest_sig(&m, &pk));
    // Strip the marker → signature must fail.
    m.distribution = None;
    assert!(!verify_manifest_sig(&m, &pk));
}
```

(If the existing tests construct manifests via a helper, extend that helper with `distribution: None` instead of inlining — match the file's idiom.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p mur-common fleet_bundle 2>&1 | tail -5`
Expected: compile error — `distribution` field missing.

- [ ] **Step 3: Add the field**

In `BundleManifest` after `entries`:

```rust
    /// `Some("official")` for bundles published from the official catalog.
    /// Inside the signed payload: stripping it invalidates the signature.
    /// Absent on user-created bundles (and skipped in serialization, so
    /// pre-existing signed bundles keep verifying byte-for-byte).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distribution: Option<String>,
```

Fix every other `BundleManifest { .. }` literal in the workspace (compiler will list them — expect `mur-core/src/cmd/fleet/export.rs` and fleet import tests) by adding `distribution: None`.

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p mur-common fleet_bundle && cargo nextest run -p mur-core fleet 2>&1 | tail -5`
Expected: all PASS (legacy import tests unaffected).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add -A mur-common/src/fleet_bundle.rs mur-core/src/cmd/fleet
git commit -m "feat(official): distribution marker in signed fleet bundle manifest"
```

---

### Task 3: `distribution` marker on `MuragentManifest`

**Files:**
- Modify: `mur-common/src/muragent/manifest.rs:9` (struct `MuragentManifest`)
- Test: inline test in `mur-common/src/muragent/manifest.rs`

**Interfaces:**
- Produces: `MuragentManifest.distribution: Option<String>`, same semantics as Task 2. The muragent signature covers `manifest.yaml` via the in-toto statement's subject hashes (`writer.rs` pins every file), so a stripped marker changes the manifest bytes and breaks subject verification — no signing-code change needed.

- [ ] **Step 1: Write failing test**

In the manifest.rs test module (create one if absent):

```rust
#[test]
fn distribution_marker_roundtrips_and_defaults_none() {
    let yaml = "schema: muragent/1\nexported_at: '2026-01-01T00:00:00Z'\nexporter: {mur_version: '1.0', tool: t}\nagent: {id: a, name: n, display_name: N, version: '1'}\nrequired_surfaces: []\nicon: {}\n";
    let m: MuragentManifest = serde_yaml_ng::from_str(yaml).expect("legacy yaml parses");
    assert!(m.distribution.is_none());
    let mut m2 = m.clone();
    m2.distribution = Some("official".into());
    let round: MuragentManifest =
        serde_yaml_ng::from_str(&serde_yaml_ng::to_string(&m2).unwrap()).unwrap();
    assert_eq!(round.distribution.as_deref(), Some("official"));
}
```

(Adjust the minimal YAML to whatever `AgentRef`/`IconHashes` actually require — check the structs at `manifest.rs:48` and fix the fixture until it parses; the assertion pair is the point.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p mur-common manifest 2>&1 | tail -5`
Expected: compile error — field missing.

- [ ] **Step 3: Add the field**

In `MuragentManifest` after `model_hint`:

```rust
    /// `Some("official")` for agents published from the official catalog.
    /// Covered by the in-toto subject hash of `manifest.yaml`, so stripping
    /// it invalidates the package signature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distribution: Option<String>,
}
```

Fix any struct literals the compiler flags (e.g. `build_manifest_from_profile` in `writer.rs:193`) with `distribution: None`.

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p mur-common muragent 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add mur-common/src/muragent
git commit -m "feat(official): distribution marker in muragent manifest"
```

---

### Task 4: License store (mur-core)

**Files:**
- Create: `mur-core/src/official/mod.rs`
- Create: `mur-core/src/official/store.rs`
- Modify: `mur-core/src/lib.rs` or `mur-core/src/main.rs` module tree — add `pub(crate) mod official;` wherever sibling top-level modules (e.g. `auth`) are declared
- Test: inline in `store.rs`

**Interfaces:**
- Consumes: `mur_common::official::{OfficialLicense, check_license, LicenseCheck}`.
- Produces:
  - `pub fn licenses_dir(mur_home: &Path) -> PathBuf` → `<mur_home>/licenses`
  - `pub fn license_path(mur_home: &Path, item: &str) -> PathBuf` → `<mur_home>/licenses/<item with '/'→"__">.yaml`
  - `pub fn save_license(mur_home: &Path, l: &OfficialLicense) -> anyhow::Result<PathBuf>` (temp file + rename, like `store/yaml.rs`)
  - `pub fn load_license(mur_home: &Path, item: &str) -> anyhow::Result<Option<OfficialLicense>>`
  - `pub fn require_license(mur_home: &Path, item: &str, user_id: &str) -> anyhow::Result<()>` — loads, runs `check_license(l, item, user_id, MUR_OFFICIAL_PUBLISHER_KEY_FP)`, errors with a distinct message per `LicenseCheck` variant; **no expiry check** (possession keeps working).

- [ ] **Step 1: `mod.rs`**

```rust
//! Official catalog client side: license store + API client.
pub mod store;
```

(`pub mod client;` is added in Task 6.)

- [ ] **Step 2: Write failing tests**

In `store.rs` tests (use `tempfile::tempdir()` as `mur_home`; sign with `ed25519_dalek::SigningKey::from_bytes(&[7u8; 32])` and pass that key's fp where needed — but `require_license` pins the REAL official fp, so for the success-path test sign with a throwaway key and assert the specific `NotOfficialKey` error message instead; full-`Ok` coverage already lives in mur-common's `check_license_matrix`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use mur_common::official::{OFFICIAL_LICENSE_FORMAT, OfficialLicense, sign_license};

    fn signed(item: &str, user: &str) -> OfficialLicense {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let mut l = OfficialLicense {
            format_version: OFFICIAL_LICENSE_FORMAT,
            user_id: user.into(),
            item: item.into(),
            version: "1.0.0".into(),
            expires_at: "2027-01-01T00:00:00Z".into(),
            signer_pubkey: String::new(),
            sig: None,
        };
        sign_license(&mut l, &key);
        l
    }

    #[test]
    fn save_load_roundtrip() {
        let home = tempfile::tempdir().unwrap();
        let l = signed("fleets/deep-research", "u1");
        save_license(home.path(), &l).unwrap();
        let got = load_license(home.path(), "fleets/deep-research").unwrap().unwrap();
        assert_eq!(got, l);
    }

    #[test]
    fn load_missing_is_none() {
        let home = tempfile::tempdir().unwrap();
        assert!(load_license(home.path(), "agents/none").unwrap().is_none());
    }

    #[test]
    fn require_license_missing_and_untrusted_signer_fail() {
        let home = tempfile::tempdir().unwrap();
        let err = require_license(home.path(), "fleets/x", "u1").unwrap_err();
        assert!(err.to_string().contains("no license"), "{err}");
        // Present but signed by a non-official key → NotOfficialKey path.
        let l = signed("fleets/x", "u1");
        save_license(home.path(), &l).unwrap();
        let err = require_license(home.path(), "fleets/x", "u1").unwrap_err();
        assert!(err.to_string().contains("not signed by the MUR official key"), "{err}");
    }

    #[test]
    fn license_path_flattens_slash() {
        let p = license_path(std::path::Path::new("/h"), "agents/researcher");
        assert_eq!(p, std::path::PathBuf::from("/h/licenses/agents__researcher.yaml"));
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo nextest run -p mur-core official::store 2>&1 | tail -5`
Expected: compile error.

- [ ] **Step 4: Implement**

```rust
//! On-disk store for official catalog licenses (`~/.mur/licenses/`).
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mur_common::official::{LicenseCheck, OfficialLicense, check_license};
use mur_common::skill::publisher_trust::MUR_OFFICIAL_PUBLISHER_KEY_FP;

pub fn licenses_dir(mur_home: &Path) -> PathBuf {
    mur_home.join("licenses")
}

/// One file per item; `/` flattened so the dir stays flat: `agents__researcher.yaml`.
pub fn license_path(mur_home: &Path, item: &str) -> PathBuf {
    licenses_dir(mur_home).join(format!("{}.yaml", item.replace('/', "__")))
}

/// Atomic write: temp file in the same dir + rename (same pattern as store/yaml.rs).
pub fn save_license(mur_home: &Path, l: &OfficialLicense) -> Result<PathBuf> {
    let dir = licenses_dir(mur_home);
    std::fs::create_dir_all(&dir).context("create licenses dir")?;
    let path = license_path(mur_home, &l.item);
    let yaml = serde_yaml_ng::to_string(l).context("serialize license")?;
    let tmp = tempfile::NamedTempFile::new_in(&dir).context("temp file")?;
    std::fs::write(tmp.path(), yaml).context("write license")?;
    tmp.persist(&path).context("persist license")?;
    Ok(path)
}

pub fn load_license(mur_home: &Path, item: &str) -> Result<Option<OfficialLicense>> {
    let path = license_path(mur_home, item);
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("read license {}", path.display()))?;
    Ok(Some(serde_yaml_ng::from_str(&text).context("parse license")?))
}

/// Fail-closed gate used by the import paths. Expiry deliberately NOT checked:
/// a lapsed subscription never disables what the user already obtained.
pub fn require_license(mur_home: &Path, item: &str, user_id: &str) -> Result<()> {
    let Some(l) = load_license(mur_home, item)? else {
        bail!("no license for {item} on this machine");
    };
    match check_license(&l, item, user_id, MUR_OFFICIAL_PUBLISHER_KEY_FP) {
        LicenseCheck::Ok => Ok(()),
        LicenseCheck::BadSignature => bail!("license for {item} has an invalid signature"),
        LicenseCheck::NotOfficialKey => {
            bail!("license for {item} is not signed by the MUR official key")
        }
        LicenseCheck::WrongUser => {
            bail!("license for {item} belongs to a different account")
        }
        LicenseCheck::WrongItem => bail!("license file mismatch for {item}"),
    }
}
```

- [ ] **Step 5: Run tests**

Run: `cargo nextest run -p mur-core official 2>&1 | tail -5`
Expected: 4 tests PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add mur-core/src/official mur-core/src/lib.rs mur-core/src/main.rs
git commit -m "feat(official): on-disk license store with fail-closed require_license"
```

---

### Task 5: Fleet import gate

**Files:**
- Modify: `mur-core/src/cmd/fleet/import.rs` — inside `cmd_fleet_import` (line 123), immediately after step 2's signature verification block (after line ~152) and before entry-hash verification
- Test: inline tests in same file (follow the existing `import_*` test idiom — they build signed bundles with helpers already in the test module)

**Interfaces:**
- Consumes: `mur_common::official::DISTRIBUTION_OFFICIAL`, `mur_common::muragent::dsse::keyid_from_pubkey`, `crate::official::store::require_license`, `crate::auth::load_tokens`.
- Produces: behavior only — official-marked fleet bundles refuse import unless (a) the bundle signature verified AND the signer key is the official key, and (b) a matching local license exists for `fleets/<fleet_name>` bound to the logged-in user.

- [ ] **Step 1: Write failing tests**

Add to the test module in `import.rs`, reusing its existing signed-bundle builder helpers (grep the module for how `import_signed_bundle_reports_signature_verified` builds a bundle; add a variant that sets `manifest.distribution = Some("official")` before signing). The auth token dependency is injected via `MUR_HOME`-relative `auth.json` — tests write one:

```rust
#[test]
fn import_official_bundle_without_license_refused() {
    let (home, bundle_path) = build_official_signed_bundle(); // helper below
    write_test_auth(&home, "user-1");
    let err = cmd_fleet_import(&home, &bundle_path, ImportOpts::default()).unwrap_err();
    assert!(err.to_string().contains("app.mur.run"), "{err}");
}

#[test]
fn import_official_bundle_with_matching_license_installs() {
    let (home, bundle_path) = build_official_signed_bundle();
    write_test_auth(&home, "user-1");
    save_official_test_license(&home, "user-1"); // license signed by SAME key as bundle
    cmd_fleet_import(&home, &bundle_path, ImportOpts::default()).unwrap();
}

#[test]
fn import_official_bundle_wrong_user_refused() {
    let (home, bundle_path) = build_official_signed_bundle();
    write_test_auth(&home, "user-2");
    save_official_test_license(&home, "user-1");
    let err = cmd_fleet_import(&home, &bundle_path, ImportOpts::default()).unwrap_err();
    assert!(err.to_string().contains("different account"), "{err}");
}
```

Helper notes (write them concretely in the test module):
- `build_official_signed_bundle()` — copy the existing signed-bundle test helper, set `distribution: Some(DISTRIBUTION_OFFICIAL.into())` before signing, return `(TempDir-as-home-PathBuf, bundle_path)`. Keep the `TempDir` guard alive (return it too if needed).
- `write_test_auth(home, user)` — write `home/auth.json` with `{"access_token":"t","refresh_token":"r","token_type":"bearer","expires_in":3600,"user_id":"<user>"}`. **Important:** `crate::auth::load_tokens` reads `crate::paths::mur_root(None)` which honors `MUR_HOME` — tests must set `MUR_HOME` to `home` for the gate call, OR (cleaner, no env races under nextest) pass the user id into the gate as a parameter. **Choose the parameter route:** see Step 3 — the gate takes `logged_in_user: Option<&str>` resolved by the caller, so tests call a small wrapper. Then `write_test_auth` is unnecessary; delete it from the tests above and pass the user directly.
- `save_official_test_license(home, user)` — build+sign an `OfficialLicense { item: "fleets/<name>", .. }` with the SAME `SigningKey` the bundle helper uses, `crate::official::store::save_license`.
- The official-fp pin: `require_license` pins the real `MUR_OFFICIAL_PUBLISHER_KEY_FP`, which test keys can't match. Add a test seam: `require_license_against(mur_home, item, user_id, official_fp)` in `store.rs` (the existing `require_license` becomes a one-line wrapper passing the const), and thread the fp through the gate the same way (`official_fp: &str` parameter on the internal gate fn, const at the call site in `cmd_fleet_import`). Same for the signer-key check.

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p mur-core import_official 2>&1 | tail -5`
Expected: compile error (helpers/gate missing).

- [ ] **Step 3: Implement the gate**

In `store.rs` add the seam:

```rust
/// Test seam: `require_license` with an explicit official fingerprint.
pub fn require_license_against(
    mur_home: &Path,
    item: &str,
    user_id: &str,
    official_fp: &str,
) -> Result<()> {
    // (move the existing require_license body here, replacing the const
    //  with `official_fp`; require_license becomes:)
    // require_license_against(mur_home, item, user_id, MUR_OFFICIAL_PUBLISHER_KEY_FP)
}
```

In `import.rs`, a free function + call site. The function (place near `confirm`):

```rust
/// Official-distribution gate. Marker present ⇒ (1) bundle must be signed by
/// `official_fp` (a self-signed bundle claiming `distribution: official` is a
/// spoof — a real license would let it impersonate official content), and
/// (2) a matching local license must exist for this item + logged-in user.
fn official_gate(
    mur_home: &Path,
    manifest: &BundleManifest,
    signer_pk: &[u8; 32],
    signature_verified: bool,
    logged_in_user: Option<&str>,
    official_fp: &str,
) -> Result<()> {
    use mur_common::muragent::dsse::keyid_from_pubkey;
    use mur_common::official::DISTRIBUTION_OFFICIAL;
    if manifest.distribution.as_deref() != Some(DISTRIBUTION_OFFICIAL) {
        return Ok(());
    }
    if !signature_verified || keyid_from_pubkey(signer_pk) != official_fp {
        bail!(
            "bundle claims official distribution but is not signed by the MUR official key — refusing import"
        );
    }
    let Some(user) = logged_in_user else {
        bail!(
            "this is official MUR content — log in (`mur login`) and get it from app.mur.run via `mur official install`"
        );
    };
    let item = format!("fleets/{}", manifest.fleet_name);
    crate::official::store::require_license_against(mur_home, &item, user, official_fp)
        .map_err(|e| {
            anyhow::anyhow!(
                "{e} — official MUR content can't be shared between accounts; get it from app.mur.run via `mur official install`"
            )
        })
}
```

Call site in `cmd_fleet_import`, right after the `signature_verified` block:

```rust
    // 2b. Official-distribution gate (fail-closed; see official_gate docs).
    let logged_in_user = crate::auth::load_tokens().and_then(|t| t.user_id);
    official_gate(
        mur_home,
        &manifest,
        &pk,
        signature_verified,
        logged_in_user.as_deref(),
        mur_common::skill::publisher_trust::MUR_OFFICIAL_PUBLISHER_KEY_FP,
    )?;
```

Tests call `official_gate` + `cmd_fleet_import` as appropriate; where the three tests above exercise the whole import, restructure them to call `official_gate` directly with explicit `logged_in_user`/`official_fp` (unit) plus keep ONE end-to-end `cmd_fleet_import` test proving the wiring exists (marker + no license + no login → error mentions app.mur.run; it does not need a fake login).

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p mur-core -E 'test(/official/)' && cargo nextest run -p mur-core -E 'test(/import_/)' 2>&1 | tail -5`
Expected: new tests PASS, all pre-existing `import_*` tests still PASS (their manifests have `distribution: None` → gate is a no-op).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add mur-core/src/cmd/fleet/import.rs mur-core/src/official/store.rs
git commit -m "feat(official): fleet import gate for official-distribution bundles"
```

---

### Task 6: Agent (.muragent) install gate

**Files:**
- Modify: `mur-core/src/cmd/agent/install.rs` — in `cmd_install` (line 25), after `validator::validate(&archive)` succeeds (line ~165)
- Test: inline tests in `install.rs` (follow the existing `cmd_install_*` test idiom, which builds `.muragent` files via `MuragentWriter`)

**Interfaces:**
- Consumes: `MuragentManifest.distribution` (Task 3), `validator::validate` result (its `keyid` field is the signer fingerprint in `ed25519-<8hex>` form already), `crate::official::store::require_license_against`.
- Produces: behavior only — official-marked `.muragent` refuses install unless signer keyid == official fp and a license for `agents/<manifest.agent.name>` matches the logged-in user.

- [ ] **Step 1: Write failing tests**

Mirror Task 5's structure. A free function `official_gate_agent` (unit-testable, fp + user injected) + one wiring test:

```rust
#[test]
fn official_agent_gate_refuses_without_license_and_passes_with() {
    let home = tempfile::tempdir().unwrap();
    // signer key used for both the "bundle" keyid and the license
    let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
    let fp = mur_common::muragent::dsse::keyid_from_pubkey(key.verifying_key().as_bytes());
    // no license → refused
    let err = official_gate_agent(home.path(), "researcher", &fp, true, Some("u1"), &fp)
        .unwrap_err();
    assert!(err.to_string().contains("app.mur.run"), "{err}");
    // matching license → passes
    let mut l = mur_common::official::OfficialLicense {
        format_version: mur_common::official::OFFICIAL_LICENSE_FORMAT,
        user_id: "u1".into(),
        item: "agents/researcher".into(),
        version: "1.0.0".into(),
        expires_at: "2027-01-01T00:00:00Z".into(),
        signer_pubkey: String::new(),
        sig: None,
    };
    mur_common::official::sign_license(&mut l, &key);
    crate::official::store::save_license(home.path(), &l).unwrap();
    official_gate_agent(home.path(), "researcher", &fp, true, Some("u1"), &fp).unwrap();
    // wrong signer keyid on the package → refused even with license
    let err = official_gate_agent(home.path(), "researcher", "ed25519-ffffffff", true, Some("u1"), &fp)
        .unwrap_err();
    assert!(err.to_string().contains("official key"), "{err}");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p mur-core official_agent_gate 2>&1 | tail -5`
Expected: compile error.

- [ ] **Step 3: Implement**

In `install.rs`:

```rust
/// Official-distribution gate for `.muragent` installs. Mirrors the fleet
/// gate: marker ⇒ signer must be the official key AND a matching local
/// license must exist. `package_keyid`/`official_fp` are `ed25519-<8hex>`.
fn official_gate_agent(
    mur_home: &Path,
    agent_name: &str,
    package_keyid: &str,
    signature_verified: bool,
    logged_in_user: Option<&str>,
    official_fp: &str,
) -> Result<()> {
    if !signature_verified || package_keyid != official_fp {
        bail!(
            "package claims official distribution but is not signed by the MUR official key — refusing install"
        );
    }
    let Some(user) = logged_in_user else {
        bail!(
            "this is official MUR content — log in (`mur login`) and get it from app.mur.run via `mur official install`"
        );
    };
    let item = format!("agents/{agent_name}");
    crate::official::store::require_license_against(mur_home, &item, user, official_fp).map_err(
        |e| {
            anyhow::anyhow!(
                "{e} — official MUR content can't be shared between accounts; get it from app.mur.run via `mur official install`"
            )
        },
    )
}
```

Call site in `cmd_install`, after the `validator::validate` match yields its result (bind the result — it carries `keyid`; check the actual `ValidationResult` field names at `mur-common/src/muragent/validator.rs:122` and whether validate-failure already bails here or is tolerated; the marker branch must only run the gate when the manifest carries it):

```rust
    // Official-distribution gate: only when the manifest carries the marker.
    let manifest: mur_common::muragent::manifest::MuragentManifest =
        serde_yaml_ng::from_str(archive.get_str("manifest.yaml")?)
            .context("parse manifest.yaml")?;
    if manifest.distribution.as_deref()
        == Some(mur_common::official::DISTRIBUTION_OFFICIAL)
    {
        let logged_in_user = crate::auth::load_tokens().and_then(|t| t.user_id);
        official_gate_agent(
            &mur_home,
            &manifest.agent.name,
            &validation.keyid,
            validation_signature_ok, // whatever bool the existing validate flow exposes
            logged_in_user.as_deref(),
            mur_common::skill::publisher_trust::MUR_OFFICIAL_PUBLISHER_KEY_FP,
        )?;
    }
```

(If `cmd_install` currently tolerates unsigned/invalid packages on some path, the marker branch must treat that as `signature_verified: false` → refusal. Read the surrounding `match validator::validate` arms and wire accordingly — fail-closed.)

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p mur-core -E 'test(/install/)' 2>&1 | tail -5`
Expected: new + pre-existing install tests PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add mur-core/src/cmd/agent/install.rs
git commit -m "feat(official): muragent install gate for official-distribution packages"
```

---

### Task 7: Catalog API client (mur-core)

**Files:**
- Create: `mur-core/src/official/client.rs`
- Modify: `mur-core/src/official/mod.rs` (add `pub mod client;`)
- Test: inline, using `wiremock` (already a mur-core dev-dep, v0.6)

**Interfaces:**
- Consumes: `mur_common::official::OfficialLicense`.
- Produces:
  - `pub struct CatalogItem { pub id: String, pub kind: String, pub name: String, pub tier: String, pub version: String, pub description: String }` (all `#[serde(default)]` except `id` — server evolves independently)
  - `pub async fn fetch_catalog(client: &reqwest::Client, base: &str) -> anyhow::Result<Vec<CatalogItem>>` → `GET {base}/api/v1/core/catalog`, expects `{"items":[...]}`
  - `pub async fn download_item(client: &reqwest::Client, base: &str, access_token: &str, id: &str) -> anyhow::Result<(Vec<u8>, OfficialLicense)>` → `GET {base}/api/v1/core/catalog/{id}/download` with `Authorization: Bearer <token>`, expects `{"license":{...},"bundle_base64":"..."}`; decodes base64, maps 401→"log in again (`mur login`)", 402/403→"requires an active MUR Pro subscription — manage at app.mur.run".

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn fetch_catalog_parses_items() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/core/catalog"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [{"id":"fleets/deep-research","kind":"fleet","name":"deep-research",
                           "tier":"pro","version":"1.0.0","description":"d"}]
            })))
            .mount(&server)
            .await;
        let items = fetch_catalog(&reqwest::Client::new(), &server.uri()).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "fleets/deep-research");
    }

    #[tokio::test]
    async fn download_decodes_bundle_and_license() {
        use base64::{Engine, engine::general_purpose::STANDARD as B64};
        let server = MockServer::start().await;
        let lic = serde_json::json!({
            "format_version":1,"user_id":"u1","item":"fleets/x","version":"1.0.0",
            "expires_at":"2027-01-01T00:00:00Z","signer_pubkey":"", "sig":"s"
        });
        Mock::given(method("GET"))
            .and(path("/api/v1/core/catalog/fleets/x/download"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "license": lic, "bundle_base64": B64.encode(b"BUNDLE")
            })))
            .mount(&server)
            .await;
        let (bytes, license) =
            download_item(&reqwest::Client::new(), &server.uri(), "tok", "fleets/x")
                .await
                .unwrap();
        assert_eq!(bytes, b"BUNDLE");
        assert_eq!(license.item, "fleets/x");
    }

    #[tokio::test]
    async fn download_maps_entitlement_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/core/catalog/fleets/x/download"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let err = download_item(&reqwest::Client::new(), &server.uri(), "tok", "fleets/x")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("subscription"), "{err}");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p mur-core official::client 2>&1 | tail -5`
Expected: compile error.

- [ ] **Step 3: Implement**

```rust
//! HTTP client for the app.mur.run official catalog API.
use anyhow::{Context, Result, bail};
use mur_common::official::OfficialLicense;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct CatalogItem {
    pub id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub tier: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Deserialize)]
struct CatalogResponse {
    items: Vec<CatalogItem>,
}

#[derive(Deserialize)]
struct DownloadResponse {
    license: OfficialLicense,
    bundle_base64: String,
}

/// Public listing — no auth.
pub async fn fetch_catalog(client: &reqwest::Client, base: &str) -> Result<Vec<CatalogItem>> {
    let url = format!("{base}/api/v1/core/catalog");
    let resp = client.get(&url).send().await.context("fetch catalog")?;
    if !resp.status().is_success() {
        bail!("catalog request failed: HTTP {}", resp.status());
    }
    Ok(resp.json::<CatalogResponse>().await.context("parse catalog")?.items)
}

/// Authenticated download: returns (bundle bytes, license).
pub async fn download_item(
    client: &reqwest::Client,
    base: &str,
    access_token: &str,
    id: &str,
) -> Result<(Vec<u8>, OfficialLicense)> {
    use base64::{Engine, engine::general_purpose::STANDARD as B64};
    let url = format!("{base}/api/v1/core/catalog/{id}/download");
    let resp = client
        .get(&url)
        .bearer_auth(access_token)
        .send()
        .await
        .context("download item")?;
    match resp.status().as_u16() {
        200 => {}
        401 => bail!("not authorized — log in again (`mur login`)"),
        402 | 403 => bail!(
            "'{id}' requires an active MUR Pro subscription — manage at app.mur.run"
        ),
        s => bail!("download failed: HTTP {s}"),
    }
    let body: DownloadResponse = resp.json().await.context("parse download response")?;
    let bytes = B64.decode(&body.bundle_base64).context("decode bundle")?;
    Ok((bytes, body.license))
}
```

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p mur-core official 2>&1 | tail -5`
Expected: all official::* tests PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add mur-core/src/official
git commit -m "feat(official): catalog API client (list + authenticated download)"
```

---

### Task 8: `mur official list|install` CLI + docs

**Files:**
- Create: `mur-core/src/cmd/official.rs`
- Modify: `mur-core/src/cli/mod.rs` — add `Official { action: OfficialAction }` variant to `Commands` (enum at line 27) with doc `/// Official MUR catalog: browse and install official agents/fleets`
- Modify: `mur-core/src/cli/actions.rs` — add `OfficialAction`
- Modify: the `Commands` dispatch `match` (same site that handles `Commands::Fleet` — find with `grep -rn "Commands::Fleet" mur-core/src`)
- Modify: `mur-core/src/cmd/mod.rs` — `pub(crate) mod official;` (the cmd module, distinct from `crate::official`)
- Modify: `CLAUDE.md` — one bullet in "CLI Surface (top level)"
- Test: CLI-parse test in `cli/mod.rs` tests + install-flow unit test in `cmd/official.rs`

**Interfaces:**
- Consumes: `crate::official::client::{fetch_catalog, download_item, CatalogItem}`, `crate::official::store::save_license`, `mur_common::official::{check_license, LicenseCheck}`, `crate::auth::{load_tokens, server_url}`, `crate::cmd::fleet::import::{cmd_fleet_import, ImportOpts}`, `crate::cmd::agent::install::cmd_install`.
- Produces:
  - `pub enum OfficialAction { List, Install { id: String } }` (clap subcommand; `id` help: `Catalog id, e.g. agents/researcher or fleets/deep-research`)
  - `pub(crate) async fn cmd_official_list() -> Result<()>`
  - `pub(crate) async fn cmd_official_install(id: &str) -> Result<()>`

- [ ] **Step 1: Write failing CLI-parse test**

In `cli/mod.rs` tests:

```rust
#[test]
fn cli_parses_official_install() {
    use clap::Parser;
    let cli = Cli::try_parse_from(["mur", "official", "install", "fleets/deep-research"]).unwrap();
    match cli.command {
        Commands::Official {
            action: crate::cli::actions::OfficialAction::Install { id },
        } => assert_eq!(id, "fleets/deep-research"),
        _ => panic!("expected official install"),
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p mur-core cli_parses_official 2>&1 | tail -5`
Expected: compile error.

- [ ] **Step 3: Implement CLI wiring + commands**

`cli/actions.rs` (follow `FleetAction`'s derive/attribute idiom exactly):

```rust
/// Official MUR catalog actions.
#[derive(clap::Subcommand, Debug)]
pub enum OfficialAction {
    /// List official agents and fleets from app.mur.run
    List,
    /// Download, verify, and install an official item (requires `mur login`)
    Install {
        /// Catalog id, e.g. agents/researcher or fleets/deep-research
        id: String,
    },
}
```

`cmd/official.rs`:

```rust
//! `mur official` — browse + install from the official MUR catalog.
use anyhow::{Context, Result, bail};
use mur_common::official::{LicenseCheck, check_license};
use mur_common::skill::publisher_trust::MUR_OFFICIAL_PUBLISHER_KEY_FP;

use crate::official::client::{download_item, fetch_catalog};
use crate::official::store::save_license;

pub(crate) async fn cmd_official_list() -> Result<()> {
    let base = crate::auth::server_url();
    let items = fetch_catalog(&reqwest::Client::new(), &base).await?;
    if items.is_empty() {
        println!("No official items published yet.");
        return Ok(());
    }
    println!("{:<32} {:<6} {:<8} {}", "ID", "TIER", "VERSION", "DESCRIPTION");
    for i in &items {
        println!("{:<32} {:<6} {:<8} {}", i.id, i.tier, i.version, i.description);
    }
    if crate::auth::load_tokens().is_none() {
        println!("\nLog in with `mur login` to install (pro items need a MUR Pro subscription).");
    }
    Ok(())
}

pub(crate) async fn cmd_official_install(id: &str) -> Result<()> {
    // 1. Identity first — everything downstream binds to it.
    let tokens = crate::auth::load_tokens()
        .context("not logged in — run `mur login` first")?;
    let user_id = tokens
        .user_id
        .clone()
        .context("stored login has no account id — run `mur auth logout` then `mur login`")?;

    // 2. Download bundle + license.
    let base = crate::auth::server_url();
    let client = reqwest::Client::new();
    let (bytes, license) = download_item(&client, &base, &tokens.access_token, id).await?;

    // 3. Verify the license fail-closed BEFORE anything touches disk state.
    match check_license(&license, id, &user_id, MUR_OFFICIAL_PUBLISHER_KEY_FP) {
        LicenseCheck::Ok => {}
        other => bail!("server returned an invalid license ({other:?}) — refusing install"),
    }

    // 4. Persist license, then hand the bundle to the existing import paths
    //    (which re-verify signatures + the official gate against this license).
    let mur_home = crate::paths::mur_root(None);
    save_license(&mur_home, &license)?;
    let dir = tempfile::tempdir().context("temp dir")?;
    match id.split_once('/') {
        Some(("fleets", name)) => {
            let p = dir.path().join(format!("{name}.fleet"));
            std::fs::write(&p, &bytes).context("write bundle")?;
            crate::cmd::fleet::import::cmd_fleet_import(
                &mur_home,
                &p,
                crate::cmd::fleet::import::ImportOpts::default(),
            )?;
        }
        Some(("agents", name)) => {
            let p = dir.path().join(format!("{name}.muragent"));
            std::fs::write(&p, &bytes).context("write package")?;
            crate::cmd::agent::install::cmd_install(&p, None, None)?;
        }
        _ => bail!("unknown catalog id '{id}' — expected agents/<name> or fleets/<name>"),
    }
    println!("✅ Installed official item {id}");
    Ok(())
}
```

Adjust visibility as the compiler demands (`cmd_fleet_import`, `ImportOpts`, `cmd_install` are `pub fn` already; module paths may need `pub(crate)` bumps). `ImportOpts` needs `#[derive(Default)]` if it lacks one. If `crate::paths::mur_root(None)` has a different signature, use whatever `cmd_login`/install already use to resolve the mur home — copy their call.

Wire the dispatch arm next to `Commands::Fleet`'s:

```rust
Commands::Official { action } => match action {
    OfficialAction::List => cmd::official::cmd_official_list().await?,
    OfficialAction::Install { id } => cmd::official::cmd_official_install(&id).await?,
},
```

(Match the surrounding dispatch style — if sibling arms return `Result` directly or block_on, do the same.)

- [ ] **Step 4: Run tests + clippy**

Run: `cargo nextest run -p mur-core -E 'test(/official/) or test(/cli_parses/)' 2>&1 | tail -5`
Expected: PASS.
Run: `cargo clippy -p mur-core -p mur-common -- -D warnings 2>&1 | tail -3`
Expected: clean.

- [ ] **Step 5: Manual smoke (no server yet — expect the clean failure modes)**

Run: `cargo run -- official list`
Expected: either a catalog listing (if the server ships first) or `catalog request failed: HTTP 404` — a clean error, no panic.
Run: `cargo run -- official install fleets/nope` (logged out)
Expected: `not logged in — run \`mur login\` first`.

- [ ] **Step 6: Update CLAUDE.md**

Add one bullet to "CLI Surface (top level)" after the `mur model` bullet:

```markdown
- `mur official {list|install <id>}` — browse and install official MUR agents/fleets from the app.mur.run catalog. Install requires `mur login`; pro-tier items require an active subscription. Downloads carry an account-bound `OfficialLicense` (stored in `~/.mur/licenses/`); the fleet/agent import paths refuse official-marked bundles without a matching license (anti-sharing gate; expiry gates downloads only, never installed content). See `docs/superpowers/specs/2026-07-20-official-catalog-design.md`.
```

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
git add mur-core/src/cmd/official.rs mur-core/src/cli mur-core/src/cmd/mod.rs CLAUDE.md
git commit -m "feat(official): mur official list|install CLI"
```

---

## Out of scope (sibling plans)

- **mur-server (Go):** `GET /api/v1/core/catalog`, `GET /api/v1/core/catalog/{id}/download` (auth + entitlement + license signing with the official key). The response shapes this plan's client expects are the contract: `{"items":[CatalogItem...]}` and `{"license":OfficialLicense-as-JSON,"bundle_base64":"..."}`.
- **`mur-run/official-catalog` repo + CI:** private repo layout + merge-triggered signing/upload job. CI must stamp `distribution: official` into manifests before signing with the official key (whose fp must equal `MUR_OFFICIAL_PUBLISHER_KEY_FP`).
- **Hub GUI store (Phase 2).**
