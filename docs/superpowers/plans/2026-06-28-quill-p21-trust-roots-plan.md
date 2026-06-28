# Quill P2.1 — Publisher trust-roots, TOFU & drift detection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn P2's "signature is internally consistent" into "signature is from a *trusted* publisher" — via a client-pinned publisher keyring + TOFU + rug-pull drift detection — without TUF or Sigstore.

**Architecture:** Add a `PublisherKeyring` (mur-common) pinned with the MUR-official key; classify a P2 `Verified` signature into `Trusted/Untrusted/Revoked`; fold that into the existing fail-closed gate (Revoked→unconditional block, Untrusted→`--yes`); pin `content_sha256`+signer at install in `SkillTrustStore` and re-prompt on drift / version rollback; add a TOFU `trust-publisher` command + Hub badge.

**Tech Stack:** Rust (reuses the existing Ed25519/DSSE stack `mur-common/src/muragent/dsse.rs`, `skill_verify.rs`, `SkillTrustStore`, `semver`); Tauri 2; React/TS.

## Global Constraints

- Builds on quill C/P2 (merged #531). Branch: `feat/quill-p21-trust-roots` off main.
- **Pinned-root + TOFU + drift only** — NO TUF, NO Sigstore keyless (would need online verify; breaks local-first).
- **Gate stays fail-closed, now three buckets:** unconditional-block (not `--yes`-overridable) = hash `Mismatch` OR signature `Invalid` OR signer `Revoked`; needs-ack (`--yes`) = signer `Untrusted` OR `Unsigned` OR absent hash OR scan-findings OR drift; clean (silent) = hash `Match` + signer `Trusted` + no findings + no drift.
- Do NOT change P2's cryptographic check (`verify_skill_install`) — trust classification is a *layer on top*.
- No hardcoded values except the one pinned root: `MUR_OFFICIAL_PUBLISHER_KEY_FP` (a deliberate compiled-in trust anchor, documented as such). Rust edition 2024. Single file ≤ 800 lines.
- Backward-compatible data: new `TrustEntry` fields use `#[serde(default)]`; `publishers.yaml` seeds on first run.
- Brand "MUR" uppercase in user-facing strings; Traditional Chinese for zh-TW i18n.
- Build/test mur-core/common: `export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"`, `ORT_STRATEGY=download`, plain `cargo test -p <crate> --lib <filter>` (NOT nextest; slow external drive — let builds finish, don't run two cargo at once). After Rust: `cargo fmt --all` + **`cargo clippy --all --no-deps -- -D warnings`** (workspace, not `--lib` — Hub-only `pub` fns appear dead → `#[allow(dead_code)]` with a one-line rationale).
- Hub: `cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml` (needs `mur-hub-gui/ui/dist/index.html` stub — gitignored, don't commit; never commit `src-tauri/binaries/*`). UI: `cd mur-hub-gui/ui && npx tsc --noEmit && npm run build` (symlink `node_modules`).

## Reused existing API (verified)

- `mur_common::muragent::dsse`: keyid format is `format!("ed25519-{}", &hex[..8])` of SHA-256(pubkey); `DsseEnvelope.signatures[0].keyid`.
- `mur-core/src/cmd/agent/skill_verify.rs`: `verify_skill_install(manifest:&SkillManifest, file_text:&str, expected_sha256:&str) -> VerifyOutcome { hash: HashStatus, signature: SignatureStatus }`; `SignatureStatus::Verified { publisher: String, key_fp: String } | Unsigned | Invalid`; `VerifyOutcome::{is_blocking, needs_ack}`.
- `mur-core/src/cmd/agent/skill_registry_add.rs`: `ConsentInfo { name, version, publisher, category, signature: SigView{status,publisher,key_fp}, hash:String, mcp_requirements, findings, blocking, needs_ack, scan_blocking, trust_level, body }`; `fn gate(consent:&ConsentInfo, accept:bool) -> Result<()>`; `resolve_consent_in(registry_dir:&Path, skill:&str, version:Option<&str>) -> Result<ConsentInfo>`; `resolve_consent(mur_home:&Path, …)`; `cmd_skill_registry_add(agent, skill, version, accept)` (calls `fetch_and_load` → `resolve_consent_in` → `gate` → `cmd_skill_add`); `registry_search_for_agent`.
- `mur-common/src/trust/skills.rs`: `SkillTrustStore { entries: BTreeMap<String,TrustEntry>, revoked: Vec<String> }` at `<mur_home>/trust/skills.json`; `TrustEntry { name, version, level: TrustLevel, installed_at: String, publisher: Option<String> }`; `load(mur_home)`, `save(mur_home)`.
- `skill_registry::{available_versions(dir,name)->Vec<semver::Version>, skill_yaml_path, fetch_and_load, DEFAULT_REGISTRY}`.
- Hub `mcp_skills.rs`: `#[tauri::command]` async fns; `agent_skill_registry_preview/install/search`; registered in `lib.rs`.

---

## File Structure

- **Create** `mur-common/src/skill/publisher_trust.rs` — `PublisherKeyring`, `TrustedPublisher`, `PublisherTrust`, `classify`, load/seed, `MUR_OFFICIAL_PUBLISHER_KEY_FP`.
- **Modify** `mur-common/src/skill/mod.rs` — `pub mod publisher_trust;` + re-exports.
- **Modify** `mur-common/src/trust/skills.rs` — add `content_sha256` + `signer_key_fp` to `TrustEntry`.
- **Create** `mur-core/src/cmd/agent/skill_signer_trust.rs` — `SignerTrust`, `classify_signer`, `check_drift` (pure).
- **Modify** `mur-core/src/cmd/agent/skill_registry_add.rs` — load keyring + classify signer + fold into blocking/needs_ack; pin on install; drift/rollback check; `ConsentInfo` gains `signer_trust`.
- **Modify** `mur-core/src/cmd/agent/mod.rs` — `pub mod skill_signer_trust;`.
- **Modify** `mur-core/src/cli/agent.rs` + `dispatch.rs` — `AgentSkillAction::TrustPublisher` + trust badge in consent printout.
- **Modify** Hub `mcp_skills.rs` + `lib.rs` + `SkillRegistryModal.tsx` + i18n — trust badge + "Trust this publisher" action.

---

## Task 1: PublisherKeyring (mur-common, pinned trust root)

**Files:** Create `mur-common/src/skill/publisher_trust.rs`; Modify `mur-common/src/skill/mod.rs`.

**Interfaces:**
- Produces: `pub enum PublisherTrust { Trusted, Revoked, Unknown }`; `pub struct TrustedPublisher { pub name: String, pub key_fp: String, pub comment: String }`; `pub struct PublisherKeyring { pub schema_version: u32, pub publishers: Vec<TrustedPublisher>, pub revoked: Vec<String> }` with `pub fn classify(&self, key_fp: &str) -> PublisherTrust`, `pub fn path(mur_home:&Path)->PathBuf`, `pub fn load_or_seed(mur_home:&Path)->Result<Self>`, `pub fn save(&self,mur_home:&Path)->Result<()>`; `pub const MUR_OFFICIAL_PUBLISHER_KEY_FP: &str`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    fn kr() -> PublisherKeyring {
        PublisherKeyring {
            schema_version: 1,
            publishers: vec![TrustedPublisher { name: "mur".into(), key_fp: "ed25519-aabbccdd".into(), comment: "official".into() }],
            revoked: vec!["ed25519-deadbeef".into()],
        }
    }
    #[test]
    fn classify_trusted_revoked_unknown() {
        let k = kr();
        assert_eq!(k.classify("ed25519-aabbccdd"), PublisherTrust::Trusted);
        assert_eq!(k.classify("ed25519-deadbeef"), PublisherTrust::Revoked);
        assert_eq!(k.classify("ed25519-00000000"), PublisherTrust::Unknown);
    }
    #[test]
    fn revoked_beats_trusted() {
        // A key both listed AND revoked must classify Revoked (fail-closed).
        let k = PublisherKeyring { schema_version:1,
            publishers: vec![TrustedPublisher{name:"x".into(),key_fp:"ed25519-aabbccdd".into(),comment:String::new()}],
            revoked: vec!["ed25519-aabbccdd".into()] };
        assert_eq!(k.classify("ed25519-aabbccdd"), PublisherTrust::Revoked);
    }
    #[test]
    fn seed_contains_official_key() {
        let dir = tempfile::tempdir().unwrap();
        let k = PublisherKeyring::load_or_seed(dir.path()).unwrap();
        assert!(k.publishers.iter().any(|p| p.key_fp == MUR_OFFICIAL_PUBLISHER_KEY_FP));
        // round-trips from disk on second load
        let k2 = PublisherKeyring::load_or_seed(dir.path()).unwrap();
        assert_eq!(k2.classify(MUR_OFFICIAL_PUBLISHER_KEY_FP), PublisherTrust::Trusted);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-common --lib publisher_trust`
Expected: FAIL — module/items not found.

- [ ] **Step 3: Write the implementation**

```rust
//! Publisher keyring — the client-pinned trust root for skill signatures.
//! Turns P2's "signature is internally consistent" into "signed by a trusted
//! publisher". Offline (SSH-known_hosts / apt-keyring style); no TUF, no Sigstore.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Fingerprint of the MUR-official publisher key, compiled in as the pinned
/// trust anchor. (Placeholder until the registry CI publishes the real key;
/// the registry-side signing task replaces this with the production fingerprint.)
pub const MUR_OFFICIAL_PUBLISHER_KEY_FP: &str = "ed25519-0fficial";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PublisherTrust {
    Trusted,
    Revoked,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedPublisher {
    pub name: String,
    pub key_fp: String,
    #[serde(default)]
    pub comment: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublisherKeyring {
    pub schema_version: u32,
    #[serde(default)]
    pub publishers: Vec<TrustedPublisher>,
    #[serde(default)]
    pub revoked: Vec<String>,
}

impl PublisherKeyring {
    pub fn path(mur_home: &Path) -> PathBuf {
        mur_home.join("trust").join("publishers.yaml")
    }

    fn seed() -> Self {
        PublisherKeyring {
            schema_version: 1,
            publishers: vec![TrustedPublisher {
                name: "mur".to_string(),
                key_fp: MUR_OFFICIAL_PUBLISHER_KEY_FP.to_string(),
                comment: "MUR official publisher (pinned trust root)".to_string(),
            }],
            revoked: Vec::new(),
        }
    }

    /// Load the keyring; if absent, seed it with the pinned official key and persist.
    pub fn load_or_seed(mur_home: &Path) -> anyhow::Result<Self> {
        let p = Self::path(mur_home);
        if p.exists() {
            let text = std::fs::read_to_string(&p)
                .map_err(|e| anyhow::anyhow!("read {}: {e}", p.display()))?;
            serde_yaml_ng::from_str(&text).map_err(|e| anyhow::anyhow!("parse keyring: {e}"))
        } else {
            let k = Self::seed();
            k.save(mur_home)?;
            Ok(k)
        }
    }

    pub fn save(&self, mur_home: &Path) -> anyhow::Result<()> {
        let p = Self::path(mur_home);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).map_err(|e| anyhow::anyhow!("mkdir: {e}"))?;
        }
        let text = serde_yaml_ng::to_string(self).map_err(|e| anyhow::anyhow!("serialize: {e}"))?;
        std::fs::write(&p, text).map_err(|e| anyhow::anyhow!("write {}: {e}", p.display()))?;
        Ok(())
    }

    /// Classify a signer key fingerprint. Revoked takes precedence (fail-closed).
    pub fn classify(&self, key_fp: &str) -> PublisherTrust {
        if self.revoked.iter().any(|r| r == key_fp) {
            PublisherTrust::Revoked
        } else if self.publishers.iter().any(|p| p.key_fp == key_fp) {
            PublisherTrust::Trusted
        } else {
            PublisherTrust::Unknown
        }
    }
}
```

Add to `mur-common/src/skill/mod.rs`: `pub mod publisher_trust;` (+ optional `pub use publisher_trust::{PublisherKeyring, PublisherTrust, TrustedPublisher, MUR_OFFICIAL_PUBLISHER_KEY_FP};`). Confirm `tempfile` is a dev-dep of mur-common (it is used elsewhere) and `serde_yaml_ng` is a dep.

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-common --lib publisher_trust` → PASS. Then `cargo clippy --all --no-deps -- -D warnings`; `cargo fmt --all`.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/skill/publisher_trust.rs mur-common/src/skill/mod.rs
git commit -m "feat(skill): publisher keyring (pinned trust root + revoke + TOFU storage)"
```

---

## Task 2: Signer trust classification + gate fold-in (mur-core)

**Files:** Create `mur-core/src/cmd/agent/skill_signer_trust.rs`; Modify `mur-core/src/cmd/agent/{mod.rs,skill_registry_add.rs}`.

**Interfaces:**
- Consumes: Task 1 `PublisherKeyring`/`PublisherTrust`; `skill_verify::SignatureStatus`.
- Produces: `pub enum SignerTrust { Trusted, Untrusted, Revoked, Unsigned, Invalid }`; `pub fn classify_signer(sig:&SignatureStatus, keyring:&PublisherKeyring) -> SignerTrust`. `ConsentInfo` gains `pub signer_trust: String` ("trusted"|"untrusted"|"revoked"|"unsigned"|"invalid"). `resolve_consent_in` gains a `keyring: &PublisherKeyring` param; folds signer trust into `blocking` (+= Revoked) and `needs_ack` (+= Untrusted). `gate()` is UNCHANGED.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::skill::publisher_trust::{PublisherKeyring, TrustedPublisher};
    use crate::cmd::agent::skill_verify::SignatureStatus;

    fn keyring(trusted: &str, revoked: &str) -> PublisherKeyring {
        PublisherKeyring { schema_version:1,
            publishers: vec![TrustedPublisher{name:"mur".into(),key_fp:trusted.into(),comment:String::new()}],
            revoked: vec![revoked.into()] }
    }
    fn verified(fp: &str) -> SignatureStatus { SignatureStatus::Verified { publisher:"mur".into(), key_fp:fp.into() } }

    #[test]
    fn verified_known_key_is_trusted() {
        let k = keyring("ed25519-trusted0", "ed25519-revoked0");
        assert_eq!(classify_signer(&verified("ed25519-trusted0"), &k), SignerTrust::Trusted);
    }
    #[test]
    fn verified_unknown_key_is_untrusted() {
        let k = keyring("ed25519-trusted0", "ed25519-revoked0");
        assert_eq!(classify_signer(&verified("ed25519-stranger"), &k), SignerTrust::Untrusted);
    }
    #[test]
    fn verified_revoked_key_is_revoked() {
        let k = keyring("ed25519-trusted0", "ed25519-revoked0");
        assert_eq!(classify_signer(&verified("ed25519-revoked0"), &k), SignerTrust::Revoked);
    }
    #[test]
    fn unsigned_and_invalid_passthrough() {
        let k = keyring("a","b");
        assert_eq!(classify_signer(&SignatureStatus::Unsigned, &k), SignerTrust::Unsigned);
        assert_eq!(classify_signer(&SignatureStatus::Invalid, &k), SignerTrust::Invalid);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-core --lib skill_signer_trust` → FAIL.

- [ ] **Step 3: Write the implementation**

```rust
//! Layer P2's `SignatureStatus` (self-consistent crypto check) onto the
//! publisher keyring to decide whether a valid signature comes from a TRUSTED
//! publisher. Pure — the gate consumes the result.

use mur_common::skill::publisher_trust::{PublisherKeyring, PublisherTrust};

use super::skill_verify::SignatureStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignerTrust {
    Trusted,   // valid signature, key in keyring
    Untrusted, // valid signature, unknown key (TOFU candidate) — needs --yes
    Revoked,   // valid signature, revoked key — unconditional block
    Unsigned,
    Invalid,
}

impl SignerTrust {
    pub fn as_str(&self) -> &'static str {
        match self {
            SignerTrust::Trusted => "trusted",
            SignerTrust::Untrusted => "untrusted",
            SignerTrust::Revoked => "revoked",
            SignerTrust::Unsigned => "unsigned",
            SignerTrust::Invalid => "invalid",
        }
    }
}

/// Classify a verified signature against the trust keyring.
pub fn classify_signer(sig: &SignatureStatus, keyring: &PublisherKeyring) -> SignerTrust {
    match sig {
        SignatureStatus::Unsigned => SignerTrust::Unsigned,
        SignatureStatus::Invalid => SignerTrust::Invalid,
        SignatureStatus::Verified { key_fp, .. } => match keyring.classify(key_fp) {
            PublisherTrust::Trusted => SignerTrust::Trusted,
            PublisherTrust::Revoked => SignerTrust::Revoked,
            PublisherTrust::Unknown => SignerTrust::Untrusted,
        },
    }
}
```

Add `pub mod skill_signer_trust;` to `mur-core/src/cmd/agent/mod.rs`.

Then in `skill_registry_add.rs`: add `pub signer_trust: String` to `ConsentInfo`; change `resolve_consent_in` to take `keyring: &PublisherKeyring`, and after building `VerifyOutcome`:

```rust
use super::skill_signer_trust::{SignerTrust, classify_signer};
// ... inside resolve_consent_in, after `let outcome = verify_skill_install(...)`:
let signer = classify_signer(&outcome.signature, keyring);
let blocking = outcome.is_blocking() || matches!(signer, SignerTrust::Revoked);
let needs_ack = outcome.needs_ack() || matches!(signer, SignerTrust::Untrusted);
// set ConsentInfo { signer_trust: signer.as_str().to_string(), blocking, needs_ack, ... }
```

Update `resolve_consent` (the wrapper) to `let keyring = PublisherKeyring::load_or_seed(mur_home)?;` then pass `&keyring` into `resolve_consent_in`. Update `cmd_skill_registry_add` similarly (it already has `mur_home`). The `gate()` fn is untouched — it reads `blocking`/`needs_ack`/`scan_blocking` which now already account for signer trust.

- [ ] **Step 4: Run tests**

Run: `ORT_STRATEGY=download cargo test -p mur-core --lib "skill_signer_trust"` and `... skill_registry_add` (update its tests to pass a fixture keyring to `resolve_consent_in`). PASS. Then `cargo clippy --all --no-deps -- -D warnings`; `cargo fmt --all`.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/skill_signer_trust.rs mur-core/src/cmd/agent/mod.rs mur-core/src/cmd/agent/skill_registry_add.rs
git commit -m "feat(skill): three-state signer trust (Trusted/Untrusted/Revoked) folded into the gate"
```

---

## Task 3: Drift pin + detection + rollback (mur-common + mur-core)

**Files:** Modify `mur-common/src/trust/skills.rs` (TrustEntry fields), `mur-core/src/cmd/agent/skill_signer_trust.rs` (pure `check_drift`), `mur-core/src/cmd/agent/skill_registry_add.rs` (pin on install + drift/rollback before gate).

**Interfaces:**
- Produces: `TrustEntry` gains `pub content_sha256: String` and `pub signer_key_fp: Option<String>` (both `#[serde(default)]`). `pub enum DriftDecision { None, Changed { what: String }, Rollback { installed: String, offered: String } }`; `pub fn check_drift(prior: Option<(&str /*hash*/, Option<&str> /*signer*/, &str /*ver*/)>, new_hash:&str, new_signer:Option<&str>, new_ver:&str) -> DriftDecision`.

- [ ] **Step 1: Write the failing test** (pure drift logic)

```rust
#[cfg(test)]
mod drift_tests {
    use super::*;
    #[test]
    fn no_prior_is_no_drift() {
        assert!(matches!(check_drift(None, "h1", Some("k1"), "1.0.0"), DriftDecision::None));
    }
    #[test]
    fn same_everything_is_no_drift() {
        assert!(matches!(check_drift(Some(("h1", Some("k1"), "1.0.0")), "h1", Some("k1"), "1.1.0"), DriftDecision::None));
    }
    #[test]
    fn changed_hash_is_drift() {
        assert!(matches!(check_drift(Some(("h1", Some("k1"), "1.0.0")), "h2", Some("k1"), "1.1.0"), DriftDecision::Changed{..}));
    }
    #[test]
    fn changed_signer_is_drift() {
        assert!(matches!(check_drift(Some(("h1", Some("k1"), "1.0.0")), "h1", Some("k2"), "1.1.0"), DriftDecision::Changed{..}));
    }
    #[test]
    fn lower_version_is_rollback() {
        assert!(matches!(check_drift(Some(("h1", Some("k1"), "2.0.0")), "h1", Some("k1"), "1.0.0"), DriftDecision::Rollback{..}));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-core --lib drift_tests` → FAIL.

- [ ] **Step 3: Write the implementation**

In `skill_signer_trust.rs`:

```rust
use semver::Version;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriftDecision {
    None,
    Changed { what: String },                       // content or signer changed
    Rollback { installed: String, offered: String },// offered version < installed
}

/// Compare a prior install record to the version about to be installed.
/// `prior` = (installed content_sha256, installed signer key_fp, installed version).
pub fn check_drift(
    prior: Option<(&str, Option<&str>, &str)>,
    new_hash: &str,
    new_signer: Option<&str>,
    new_ver: &str,
) -> DriftDecision {
    let Some((old_hash, old_signer, old_ver)) = prior else { return DriftDecision::None };
    // Rollback: offered semver strictly lower than installed.
    if let (Ok(o), Ok(n)) = (Version::parse(old_ver), Version::parse(new_ver))
        && n < o
    {
        return DriftDecision::Rollback { installed: old_ver.to_string(), offered: new_ver.to_string() };
    }
    if !new_hash.is_empty() && !old_hash.is_empty() && new_hash != old_hash {
        return DriftDecision::Changed { what: "content".to_string() };
    }
    if old_signer.is_some() && new_signer != old_signer {
        return DriftDecision::Changed { what: "publisher".to_string() };
    }
    DriftDecision::None
}
```

In `mur-common/src/trust/skills.rs`, extend `TrustEntry`:

```rust
    #[serde(default)]
    pub content_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_key_fp: Option<String>,
```
(Update any `TrustEntry { .. }` literal constructors in that file to include the new fields, or add `..Default::default()` if it derives Default — confirm and adapt.)

In `skill_registry_add.rs::cmd_skill_registry_add`, BEFORE `gate(&consent, accept)`:

```rust
// Rug-pull / rollback: compare to the prior install record (if any).
let store = mur_common::trust::skills::SkillTrustStore::load(&mur_home).unwrap_or_default();
let prior = store.entries.values().find(|e| e.name == consent.name);
let prior_tuple = prior.map(|e| (e.content_sha256.as_str(), e.signer_key_fp.as_deref(), e.version.as_str()));
let new_signer_fp = if consent.signature.status == "verified" { Some(consent.signature.key_fp.as_str()) } else { None };
match check_drift(prior_tuple, /*new_hash=*/&resolved_sha256, new_signer_fp, &consent.version) {
    DriftDecision::None => {}
    DriftDecision::Changed { what } if !accept => bail!(
        "'{}' {} changed since you installed it — re-run with --yes to reinstall.", consent.name, what),
    DriftDecision::Rollback { installed, offered } if !accept => bail!(
        "refusing to downgrade '{}' {installed} → {offered} (rollback) — re-run with --yes to override.", consent.name),
    _ => {}
}

gate(&consent, accept)?;
// ... after cmd_skill_add succeeds, pin the record:
let mut store = mur_common::trust::skills::SkillTrustStore::load(&mur_home).unwrap_or_default();
store.entries.insert(consent.name.clone(), mur_common::trust::skills::TrustEntry {
    name: consent.name.clone(),
    version: consent.version.clone(),
    level: mur_common::skill::TrustLevel::Sandboxed,
    installed_at: /* reuse the existing timestamp helper used elsewhere in the crate */ now_rfc3339(),
    publisher: Some(consent.publisher.clone()),
    content_sha256: resolved_sha256.clone(),
    signer_key_fp: new_signer_fp.map(|s| s.to_string()),
});
let _ = store.save(&mur_home);
```
(`resolved_sha256` = sha256 of the resolved file text — compute once where the file is read, or expose it on `ConsentInfo` as `pub resolved_sha256: String`; cleanest is to add that field in Task 2's `resolve_consent_in` since it already reads the file. Confirm the crate's existing RFC3339 timestamp helper — `cmd_skill_add`/TrustEntry construction elsewhere uses one — and reuse it rather than adding a dep.)

- [ ] **Step 4: Run tests**

Run: `ORT_STRATEGY=download cargo test -p mur-core --lib "skill_signer_trust"` + `cargo test -p mur-common --lib trust` + `... skill_registry_add`. PASS. Then `cargo clippy --all --no-deps -- -D warnings`; `cargo fmt --all`.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/trust/skills.rs mur-core/src/cmd/agent/skill_signer_trust.rs mur-core/src/cmd/agent/skill_registry_add.rs
git commit -m "feat(skill): pin content+signer on install; rug-pull drift + rollback re-prompt"
```

---

## Task 4: CLI — trust-publisher (TOFU) + trust badge in consent

**Files:** Modify `mur-core/src/cli/agent.rs` (`AgentSkillAction::TrustPublisher`), `mur-core/src/dispatch.rs`.

**Interfaces:** Consumes Task 1 `PublisherKeyring`; Task 2 `ConsentInfo.signer_trust`.

- [ ] **Step 1: Add the CLI variant** in `enum AgentSkillAction` (after `Search`):

```rust
    /// Trust a skill publisher key (TOFU) — adds it to ~/.mur/trust/publishers.yaml
    /// so future installs signed by this key are treated as Trusted.
    TrustPublisher {
        /// Publisher key fingerprint (e.g. `ed25519-abcd1234`), shown in the
        /// install consent screen for an unknown-but-verified signer.
        key_fp: String,
        /// Friendly name to record alongside the key.
        #[arg(long)]
        name: Option<String>,
    },
```

- [ ] **Step 2: Wire dispatch** in `dispatch.rs` (next to the other `AgentSkillAction` arms):

```rust
            AgentSkillAction::TrustPublisher { key_fp, name } => {
                let mur_home = cmd::agent::resolve_mur_home()?;
                let mut kr = mur_common::skill::publisher_trust::PublisherKeyring::load_or_seed(&mur_home)?;
                if kr.revoked.iter().any(|r| *r == key_fp) {
                    anyhow::bail!("refusing to trust a revoked key: {key_fp}");
                }
                if !kr.publishers.iter().any(|p| p.key_fp == key_fp) {
                    kr.publishers.push(mur_common::skill::publisher_trust::TrustedPublisher {
                        name: name.clone().unwrap_or_else(|| "user-trusted".to_string()),
                        key_fp: key_fp.clone(),
                        comment: "added via trust-publisher (TOFU)".to_string(),
                    });
                    kr.save(&mur_home)?;
                    println!("Trusted publisher {key_fp}. Skills signed by this key now install without --yes.");
                } else {
                    println!("Publisher {key_fp} is already trusted.");
                }
            }
```

Also augment the existing `RegistryAdd` consent printout to show the signer trust: in the consent-print block add `println!("Trust:     {}", c.signer_trust);` (so an unknown signer is visible, with the `key_fp` already printed by P2).

- [ ] **Step 3: Build + verify**

Run: `ORT_STRATEGY=download cargo build -p mur-core --bin mur` then `./target/debug/mur agent skill trust-publisher --help`. Then `cargo clippy --all --no-deps -- -D warnings`; `cargo fmt --all`.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cli/agent.rs mur-core/src/dispatch.rs
git commit -m "feat(cli): mur agent skill trust-publisher (TOFU) + signer-trust in consent"
```

---

## Task 5: Hub — trust badge + "Trust this publisher" action

**Files:** Modify `mur-hub-gui/src-tauri/src/{mcp_skills.rs,lib.rs}`, `mur-hub-gui/ui/src/components/SkillRegistryModal.tsx`, i18n `en.ts`/`zh-TW.ts`.

**Interfaces:** Consumes Task 1 keyring; Task 2 `ConsentInfo.signer_trust`.

- [ ] **Step 1: Tauri command** in `mcp_skills.rs`:

```rust
/// Trust a publisher key (TOFU) from the Hub consent screen.
#[tauri::command]
pub async fn agent_skill_trust_publisher(key_fp: String, name: Option<String>) -> Result<(), String> {
    let home = mur_core::cmd::agent::resolve_mur_home().map_err(|e| format!("{e:#}"))?;
    let mut kr = mur_common::skill::publisher_trust::PublisherKeyring::load_or_seed(&home).map_err(|e| format!("{e:#}"))?;
    if kr.revoked.iter().any(|r| *r == key_fp) { return Err(format!("key {key_fp} is revoked")); }
    if !kr.publishers.iter().any(|p| p.key_fp == key_fp) {
        kr.publishers.push(mur_common::skill::publisher_trust::TrustedPublisher {
            name: name.unwrap_or_else(|| "user-trusted".to_string()), key_fp, comment: "TOFU (Hub)".to_string() });
        kr.save(&home).map_err(|e| format!("{e:#}"))?;
    }
    Ok(())
}
```
Register it in `lib.rs`'s `generate_handler![]`. (`ConsentInfo` already serialises `signer_trust` from Task 2 — no backend change to preview/install.)

- [ ] **Step 2: Build** — `cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml` (stub dist; don't commit) then clippy + fmt for that manifest.

- [ ] **Step 3: UI** in `SkillRegistryModal.tsx` consent view: add a trust badge from `consent.signer_trust` (`trusted` → `✓ {t("skillreg.trusted")}`, `untrusted` → `⚠ {t("skillreg.untrusted")}` + a "Trust this publisher" button calling `invoke("agent_skill_trust_publisher", { keyFp: consent.signature.key_fp, name: consent.publisher })` then re-preview, `revoked` → `✗ {t("skillreg.revoked")}`). Add the `signer_trust` field to the TS `ConsentInfo` type. Gating already follows `blocking`/`needs_ack` from Task 2 (no change). i18n: add `skillreg.{trusted,untrusted,revoked,trustPublisher,trusted_done}` to BOTH `en.ts` + `zh-TW.ts` (Traditional Chinese).

- [ ] **Step 4: Typecheck + build** — `cd mur-hub-gui/ui && npx tsc --noEmit && npm run build` (tsc 0; vite ok).

- [ ] **Step 5: Commit**

```bash
git add mur-hub-gui/src-tauri/src/mcp_skills.rs mur-hub-gui/src-tauri/src/lib.rs mur-hub-gui/ui/src/components/SkillRegistryModal.tsx mur-hub-gui/ui/src/i18n/en.ts mur-hub-gui/ui/src/i18n/zh-TW.ts
git commit -m "feat(hub): publisher trust badge + Trust-this-publisher (TOFU) action"
```

- [ ] **Step 6: Live verify (manual)**

Extend the P2 fixture registry with three signed skills: one signed by a key in `publishers.yaml` (installs silently — Trusted), one by an unknown key (needs `--yes`; `trust-publisher` then makes it silent — TOFU), one by a key listed in `revoked` (blocked unconditionally). Then reinstall a skill whose content changed (drift re-prompt) and a lower version (rollback refusal). Run via `mur agent skill registry-add` + the Hub Browse flow.

---

## Self-Review

**Spec coverage:** §3.1 keyring → Task 1 ✓; §3.2 three-state signer trust → Task 2 ✓; §3.3 gate (Revoked→block, Untrusted→ack) → Task 2 folds into blocking/needs_ack, `gate()` unchanged ✓; §3.4 TOFU → Task 4 (CLI) + Task 5 (Hub) ✓; §3.5 drift pin + detection + rollback → Task 3 ✓; §3.6 registry CI → spec companion (out of plan scope, noted) ✓; §6 UX badges → Task 4/5 ✓. Revocation list (embedded + registry-fetched) — embedded via keyring `revoked` (Task 1); registry-fetched union is a noted follow-on (open question §9, not a task).

**Placeholder scan:** the "confirm" notes (TrustEntry literal constructors, the crate's RFC3339 timestamp helper, `resolved_sha256` exposure) each name the exact file/symbol to check — implementer guidance, not vague placeholders. `MUR_OFFICIAL_PUBLISHER_KEY_FP` is a documented placeholder fingerprint the registry-CI companion replaces (called out in Global Constraints + the const doc-comment).

**Type consistency:** `PublisherKeyring::classify -> PublisherTrust{Trusted,Revoked,Unknown}` (Task 1) consumed by `classify_signer -> SignerTrust{Trusted,Untrusted,Revoked,Unsigned,Invalid}` (Task 2); `SignerTrust` folded into `ConsentInfo.{blocking,needs_ack,signer_trust:String}` (Task 2) → surfaced CLI (Task 4) + Hub (Task 5). `check_drift -> DriftDecision{None,Changed,Rollback}` (Task 3) consumed in `cmd_skill_registry_add`. `TrustEntry.{content_sha256,signer_key_fp}` (Task 3) written on install + read for drift. `resolve_consent_in(registry_dir, skill, version, keyring)` signature consistent across Task 2/3 and the `resolve_consent` wrapper.
