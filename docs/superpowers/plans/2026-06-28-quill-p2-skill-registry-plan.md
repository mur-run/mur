# Quill C (P2) — Per-agent skill registry install Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Install a registry skill onto a specific agent (`mur agent skill registry-add <agent> <name>` + Hub Browse), with verify-on-install (DSSE signature + content-hash, fail-closed) and a transparent consent screen.

**Architecture:** Reuse the EXISTING git registry (`cmd/skill_registry.rs`), resolver, DSSE sign primitives (`mur-common/src/skill/sign.rs`), and per-agent installer (`cmd/agent/skill.rs::cmd_skill_add`). Add a network-free `skill_verify` module (the first real consumer of `verify_manifest` + `content_sha256`), a per-agent `skill_registry_add` command that wires resolve → verify → install → Sandboxed trust, CLI verbs, and Hub commands + a Browse view.

**Tech Stack:** Rust (reuses `semver`, `sha2`+`hex`, `serde_json`, existing `skill_registry`/`skill_resolver`/`skill::sign`/`skill::local`); Tauri 2; React/TS.

## Global Constraints

- Builds on the EXISTING registry; do NOT build a new one or change the index schema (`RegistrySkillEntry.content_sha256` already exists).
- **Verify-on-install is fail-closed:** a content-hash **mismatch** or an **invalid** signature aborts the install unless `--yes`/explicit accept. **Absent** hash or **unsigned** → warn + require `--yes` (so it still works against today's unsigned registry). Never silently install on a verify failure.
- Registry skills install at **`TrustLevel::Sandboxed`** (least privilege).
- Consent (CLI + Hub) shows publisher, signature status, hash status, trust level, declared MCP requirements, scan findings, and the full body BEFORE install; nothing acted on pre-consent.
- Reuse `cmd::agent::skill::cmd_skill_add` for the actual write (it already validates + scans). Mirror feather's `mcp_registry::cmd_mcp_registry_add(agent, server_name)` shape.
- No hardcoded magic values (consts). Rust edition 2024 (let-chains ok). Single file ≤ 800 lines.
- Brand "MUR" uppercase in user-facing strings; Traditional Chinese for zh-TW i18n.
- Build/test mur-core: `export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"`, `ORT_STRATEGY=download`, plain `cargo test -p mur-core --lib <filter>` (NOT nextest; slow external drive — let builds finish, don't run two cargo at once). After Rust changes: `cargo fmt --all`. **Clippy check with `cargo clippy --all --no-deps -- -D warnings`** (NOT just `--lib`) — Hub-only `pub` fns appear dead in the workspace build, so any fn consumed ONLY by `mur-hub-gui` (workspace-excluded) needs `#[allow(dead_code)]` with a one-line rationale (see quill bundles).
- Hub: `cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml` (needs a `mur-hub-gui/ui/dist/index.html` stub — gitignored, don't commit; never commit the tracked 0-byte `src-tauri/binaries/*` stubs). UI: `cd mur-hub-gui/ui && npx tsc --noEmit && npm run build` (symlink `node_modules` from the main checkout).

## Reused existing API (verified)

- `cmd::skill_registry::{DEFAULT_REGISTRY, fetch_and_load(home,url)->(PathBuf,RegistryIndex), skill_yaml_path(dir,name,ver)->PathBuf, available_versions(dir,name)->Vec<Version>, search_registry(&idx,query)->Vec<(&str,&RegistrySkillEntry)>}`.
- `mur_common::skill::registry::{RegistryIndex{schema_version,updated_at,skills:BTreeMap<String,RegistrySkillEntry>}, RegistrySkillEntry{latest,description,publisher,category,tags,content_sha256,install_count}}`.
- `mur_common::skill::{parse_canonical(&str)->Result<SkillManifest>, validate(&m), scan::scan_skill(&m)->ContentScanReport}`.
- `mur_common::skill::sign::{verify_manifest(&SkillManifest,&str)->Result<(),SignError>, sign_manifest(&SkillManifest,&AgentIdentity)->Result<String,SignError>, SKILL_PAYLOAD_TYPE}`.
- `mur_common::muragent::dsse::DsseEnvelope{payload_type,payload,signatures:Vec<DsseSignature{keyid,sig}>}` (Deserialize).
- `SkillManifest{name,version,publisher,category:Category,publisher_signature:Option<String>,mcp_requirements:Vec<McpRequirement>, content...}`.
- `mur_common::skill::TrustLevel` (variant `Sandboxed`); `mur_common::skill::local::set_trust_level(home,name,level)` / `get_trust_level`.
- `cmd::agent::skill::cmd_skill_add(agent:&str, source:&str)->Result<()>` (writes `agents/<agent>/skills/<manifest.name>/skill.yaml`; validates + scans).
- `cmd::agent::resolve_mur_home()->Result<PathBuf>`.
- `AgentSkillAction` enum (`mur-core/src/cli/agent.rs`): variants List/Add/Remove/Show/Enable/Disable/AddUrl. Dispatched in `mur-core/src/dispatch.rs` (`AgentSkillAction::AddUrl{name,url,yes}=>...`).
- Hub `mcp_skills.rs`: `#[tauri::command]` async fns returning `Result<_,String>`, `get_agent_detail(name)->Result<AgentDetail,String>`; commands registered in `mur-hub-gui/src-tauri/src/lib.rs`.

---

## File Structure

- **Create** `mur-core/src/cmd/agent/skill_verify.rs` — verify-on-install (hash + DSSE signature) → `VerifyOutcome`. Network-free.
- **Create** `mur-core/src/cmd/agent/skill_registry_add.rs` — `cmd_skill_registry_add` + `registry_search_for_agent` + the consent struct.
- **Modify** `mur-core/src/cmd/agent/mod.rs` — `pub mod skill_verify; pub mod skill_registry_add;`.
- **Modify** `mur-core/src/cli/agent.rs` — `AgentSkillAction::{RegistryAdd, Search}`.
- **Modify** `mur-core/src/dispatch.rs` — wire the two actions (print consent + confirm).
- **Modify** `mur-hub-gui/src-tauri/src/mcp_skills.rs` + `lib.rs` — `agent_skill_registry_search`, `agent_skill_registry_install`.
- **Modify** Hub UI (`mur-hub-gui/ui/src/components/`) — "Browse registry" view + consent reuse; i18n.

---

## Task 1: `skill_verify.rs` — verify-on-install (the security core)

**Files:** Create `mur-core/src/cmd/agent/skill_verify.rs`; Modify `mur-core/src/cmd/agent/mod.rs`.

**Interfaces:**
- Produces: `pub enum HashStatus{Match,Mismatch,Absent}`; `pub enum SignatureStatus{Verified{publisher:String,key_fp:String},Unsigned,Invalid}`; `pub struct VerifyOutcome{pub hash:HashStatus,pub signature:SignatureStatus}` with `pub fn is_blocking(&self)->bool` and `pub fn needs_ack(&self)->bool`; `pub fn verify_skill_install(manifest:&SkillManifest, file_text:&str, expected_sha256:&str)->VerifyOutcome`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::skill::{parse_canonical, sign::sign_manifest};
    use mur_common::identity::AgentIdentity;

    const CLEAN: &str = "name: reg-skill\nversion: 1.0.0\npublisher: human:test\ndescription: d\ncategory: context\ncontent:\n  abstract: x\n  context: y\n";

    fn sha(s: &str) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(s.as_bytes()))
    }

    #[test]
    fn hash_match_unsigned_is_nonblocking_but_needs_ack() {
        let m = parse_canonical(CLEAN).unwrap();
        let o = verify_skill_install(&m, CLEAN, &sha(CLEAN));
        assert!(matches!(o.hash, HashStatus::Match));
        assert!(matches!(o.signature, SignatureStatus::Unsigned));
        assert!(!o.is_blocking());      // unsigned is not a hard block
        assert!(o.needs_ack());          // but unsigned requires --yes
    }

    #[test]
    fn hash_mismatch_is_blocking() {
        let m = parse_canonical(CLEAN).unwrap();
        let o = verify_skill_install(&m, CLEAN, &sha("different bytes"));
        assert!(matches!(o.hash, HashStatus::Mismatch));
        assert!(o.is_blocking());
    }

    #[test]
    fn absent_hash_needs_ack_not_block() {
        let m = parse_canonical(CLEAN).unwrap();
        let o = verify_skill_install(&m, CLEAN, "");
        assert!(matches!(o.hash, HashStatus::Absent));
        assert!(!o.is_blocking());
        assert!(o.needs_ack());
    }

    #[test]
    fn valid_signature_verifies() {
        let id = AgentIdentity::generate();           // confirm constructor in identity.rs
        let mut m = parse_canonical(CLEAN).unwrap();
        let envelope = sign_manifest(&m, &id).unwrap();
        m.publisher_signature = Some(envelope);
        let o = verify_skill_install(&m, CLEAN, &sha(CLEAN));
        assert!(matches!(o.signature, SignatureStatus::Verified { .. }));
        assert!(!o.is_blocking());
        assert!(!o.needs_ack());                       // signed + hash-match = clean
    }

    #[test]
    fn tampered_signature_is_invalid_and_blocking() {
        let id = AgentIdentity::generate();
        let mut m = parse_canonical(CLEAN).unwrap();
        let envelope = sign_manifest(&m, &id).unwrap();
        m.publisher_signature = Some(envelope);
        m.description = "TAMPERED after signing".into();  // invalidates the signature
        let o = verify_skill_install(&m, CLEAN, &sha(CLEAN));
        assert!(matches!(o.signature, SignatureStatus::Invalid));
        assert!(o.is_blocking());
    }
}
```

(Confirm the real `AgentIdentity` constructor — `AgentIdentity::generate()` or similar — in `mur-common/src/identity.rs`; adapt the test if the name differs.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `ORT_STRATEGY=download cargo test -p mur-core --lib skill_verify`
Expected: FAIL — module/items not found.

- [ ] **Step 3: Write the implementation**

```rust
//! Verify-on-install for registry skills (fail-closed): content-hash pin +
//! DSSE/Ed25519 publisher signature. The first real consumer of
//! `mur_common::skill::sign::verify_manifest` + `RegistrySkillEntry.content_sha256`.

use mur_common::muragent::dsse::DsseEnvelope;
use mur_common::skill::SkillManifest;
use mur_common::skill::sign::verify_manifest;

/// Result of comparing the resolved skill file against the registry's pinned hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HashStatus {
    Match,
    Mismatch,
    Absent, // registry entry carried no content_sha256
}

/// Result of verifying the manifest's DSSE publisher signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureStatus {
    Verified { publisher: String, key_fp: String },
    Unsigned,
    Invalid,
}

#[derive(Debug, Clone)]
pub struct VerifyOutcome {
    pub hash: HashStatus,
    pub signature: SignatureStatus,
}

impl VerifyOutcome {
    /// Hard failure — a positive sign of tampering. Abort unless explicitly accepted.
    pub fn is_blocking(&self) -> bool {
        matches!(self.hash, HashStatus::Mismatch) || matches!(self.signature, SignatureStatus::Invalid)
    }
    /// Not proven-bad, but not proven-good (unsigned / unhashed). Require `--yes`.
    pub fn needs_ack(&self) -> bool {
        !self.is_blocking()
            && (matches!(self.hash, HashStatus::Absent)
                || matches!(self.signature, SignatureStatus::Unsigned))
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

/// Verify a resolved skill file against the registry's pinned hash and the
/// manifest's embedded DSSE signature. Pure — no I/O, no install.
pub fn verify_skill_install(
    manifest: &SkillManifest,
    file_text: &str,
    expected_sha256: &str,
) -> VerifyOutcome {
    let hash = if expected_sha256.is_empty() {
        HashStatus::Absent
    } else if sha256_hex(file_text.as_bytes()).eq_ignore_ascii_case(expected_sha256) {
        HashStatus::Match
    } else {
        HashStatus::Mismatch
    };

    let signature = match &manifest.publisher_signature {
        None => SignatureStatus::Unsigned,
        Some(envelope) => match verify_manifest(manifest, envelope) {
            Ok(()) => {
                let key_fp = serde_json::from_str::<DsseEnvelope>(envelope)
                    .ok()
                    .and_then(|e| e.signatures.first().map(|s| s.keyid.clone()))
                    .unwrap_or_default();
                SignatureStatus::Verified {
                    publisher: manifest.publisher.clone(),
                    key_fp,
                }
            }
            Err(_) => SignatureStatus::Invalid,
        },
    };

    VerifyOutcome { hash, signature }
}
```

Add to `mur-core/src/cmd/agent/mod.rs`: `pub mod skill_verify;`

- [ ] **Step 4: Run tests to verify they pass**

Run: `ORT_STRATEGY=download cargo test -p mur-core --lib skill_verify` → PASS. Then `cargo clippy --all --no-deps -- -D warnings` clean (add `#[allow(dead_code)]` to any item only used later by the Hub) and `cargo fmt --all`.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/skill_verify.rs mur-core/src/cmd/agent/mod.rs
git commit -m "feat(skill): verify-on-install (content-hash + DSSE signature), fail-closed"
```

---

## Task 2: `skill_registry_add.rs` — per-agent install + search

**Files:** Create `mur-core/src/cmd/agent/skill_registry_add.rs`; Modify `mur-core/src/cmd/agent/mod.rs`.

**Interfaces:**
- Consumes: Task 1 `verify_skill_install`/`VerifyOutcome`; `skill_registry::*`; `cmd::agent::skill::cmd_skill_add`; `skill::local::set_trust_level`.
- Produces:
  - `pub struct ConsentInfo{pub name:String,pub version:String,pub publisher:String,pub category:String,pub signature:SignatureStatus,pub hash:HashStatus,pub mcp_requirements:Vec<String>,pub findings:Vec<String>,pub body:String}`.
  - `pub fn resolve_consent(mur_home:&Path, skill:&str, version:Option<&str>)->Result<ConsentInfo>` — fetch+resolve+verify+scan; installs nothing (Hub preview + CLI confirm both use this).
  - `pub async fn cmd_skill_registry_add(agent:&str, skill:&str, version:Option<&str>, accept:bool)->Result<String>` — resolve_consent → fail-closed gate → `cmd_skill_add` → set `TrustLevel::Sandboxed` → returns `"skills/<name>"`.
  - `pub fn registry_search_for_agent(mur_home:&Path, query:&str)->Result<Vec<RegistrySkillEntryView>>` (a serializable view: `{name,description,publisher,category,latest,signed_in_index:bool}`).

- [ ] **Step 1: Write the failing test** (integration, against a local fixture registry dir — no network/git)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // Lay out a temp dir exactly like the cloned registry: index.yaml + skills/<n>/versions/<v>.yaml
    fn fixture_registry(dir: &std::path::Path, skill_yaml: &str, sha256: &str) {
        let idx = format!(
            "schema_version: 1\nupdated_at: '2026-06-28'\nskills:\n  reg-skill:\n    latest: 1.0.0\n    description: d\n    publisher: human:test\n    category: context\n    content_sha256: '{sha256}'\n    install_count: 0\n"
        );
        fs::write(dir.join("index.yaml"), idx).unwrap();
        let vdir = dir.join("skills").join("reg-skill").join("versions");
        fs::create_dir_all(&vdir).unwrap();
        fs::write(vdir.join("1.0.0.yaml"), skill_yaml).unwrap();
    }

    fn sha(s: &str) -> String { use sha2::{Digest, Sha256}; hex::encode(Sha256::digest(s.as_bytes())) }

    const SKILL: &str = "name: reg-skill\nversion: 1.0.0\npublisher: human:test\ndescription: d\ncategory: context\ncontent:\n  abstract: x\n  context: y\n";

    #[test]
    fn resolve_consent_reports_hash_match_and_unsigned() {
        let dir = tempfile::tempdir().unwrap();
        fixture_registry(dir.path(), SKILL, &sha(SKILL));
        let c = resolve_consent_in(dir.path(), "reg-skill", None).unwrap(); // test seam: takes registry_dir directly
        assert_eq!(c.name, "reg-skill");
        assert!(matches!(c.hash, super::HashStatus::Match));
        assert!(matches!(c.signature, super::SignatureStatus::Unsigned));
    }

    #[test]
    fn resolve_consent_detects_tampered_hash() {
        let dir = tempfile::tempdir().unwrap();
        fixture_registry(dir.path(), SKILL, &sha("wrong"));
        let c = resolve_consent_in(dir.path(), "reg-skill", None).unwrap();
        assert!(matches!(c.hash, super::HashStatus::Mismatch));
    }
}
```

(Provide a `resolve_consent_in(registry_dir, skill, version)` test seam that `resolve_consent` calls after `fetch_and_load`, so tests don't hit git. `cmd_skill_registry_add` is exercised end-to-end in the manual/live step — it needs a real agent profile.)

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-core --lib skill_registry_add` → FAIL (items not found).

- [ ] **Step 3: Write the implementation**

```rust
//! Install a registry skill onto a specific agent — the per-agent sibling of
//! `mcp_registry::cmd_mcp_registry_add`. Reuses the existing git registry +
//! resolver + per-agent installer, adding verify-on-install + Sandboxed trust.

use std::path::Path;

use anyhow::{Result, bail};
use semver::Version;

use super::skill_verify::{HashStatus, SignatureStatus, VerifyOutcome, verify_skill_install};
use crate::cmd::skill_registry;
use mur_common::skill::{TrustLevel, parse_canonical, scan::scan_skill};

#[derive(Debug, Clone, serde::Serialize)]
pub struct ConsentInfo {
    pub name: String,
    pub version: String,
    pub publisher: String,
    pub category: String,
    pub signature: SigView,
    pub hash: String, // "match" | "mismatch" | "absent"
    pub mcp_requirements: Vec<String>,
    pub findings: Vec<String>,
    pub blocking: bool,
    pub needs_ack: bool,
    pub body: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SigView {
    pub status: String, // "verified" | "unsigned" | "invalid"
    pub publisher: String,
    pub key_fp: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RegistrySkillEntryView {
    pub name: String,
    pub description: String,
    pub publisher: String,
    pub category: String,
    pub latest: String,
    pub signed_in_index: bool,
}

fn hash_str(h: &HashStatus) -> &'static str {
    match h {
        HashStatus::Match => "match",
        HashStatus::Mismatch => "mismatch",
        HashStatus::Absent => "absent",
    }
}

fn sig_view(s: &SignatureStatus) -> SigView {
    match s {
        SignatureStatus::Verified { publisher, key_fp } => SigView {
            status: "verified".into(),
            publisher: publisher.clone(),
            key_fp: key_fp.clone(),
        },
        SignatureStatus::Unsigned => SigView { status: "unsigned".into(), publisher: String::new(), key_fp: String::new() },
        SignatureStatus::Invalid => SigView { status: "invalid".into(), publisher: String::new(), key_fp: String::new() },
    }
}

/// Build the consent screen for a registry skill. Installs nothing. (Test seam.)
pub fn resolve_consent_in(registry_dir: &Path, skill: &str, version: Option<&str>) -> Result<ConsentInfo> {
    let idx = skill_registry::load_index(registry_dir)?;
    let entry = idx
        .skills
        .get(skill)
        .ok_or_else(|| anyhow::anyhow!("skill '{skill}' not found in registry"))?;
    let ver = match version {
        Some(v) => v.to_string(),
        None => entry.latest.clone(),
    };
    // Confirm the version exists.
    let avail = skill_registry::available_versions(registry_dir, skill)?;
    if !avail.iter().any(|v| v.to_string() == ver) {
        // allow if it equals `latest` even when versions dir layout differs
        if ver != entry.latest {
            bail!("version '{ver}' of '{skill}' not in registry (have: {avail:?})");
        }
    }
    let _ = Version::parse(&ver); // tolerate non-semver per registry policy
    let path = skill_registry::skill_yaml_path(registry_dir, skill, &ver);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
    let manifest = parse_canonical(&text).map_err(|e| anyhow::anyhow!("parse skill: {e}"))?;
    let outcome: VerifyOutcome = verify_skill_install(&manifest, &text, &entry.content_sha256);
    let report = scan_skill(&manifest).map_err(|e| anyhow::anyhow!("scan: {e}"))?;
    Ok(ConsentInfo {
        name: manifest.name.clone(),
        version: ver,
        publisher: entry.publisher.clone(),
        category: entry.category.clone(),
        signature: sig_view(&outcome.signature),
        hash: hash_str(&outcome.hash).into(),
        mcp_requirements: manifest.mcp_requirements.iter().map(|r| format!("{r:?}")).collect(),
        findings: report.findings.iter().map(|f| f.to_string()).collect(), // confirm ContentScanReport.findings shape
        blocking: outcome.is_blocking(),
        needs_ack: outcome.needs_ack(),
        body: text,
    })
}

/// Fetch the registry (git), then build consent.
pub fn resolve_consent(mur_home: &Path, skill: &str, version: Option<&str>) -> Result<ConsentInfo> {
    let (dir, _idx) = skill_registry::fetch_and_load(mur_home, skill_registry::DEFAULT_REGISTRY)?;
    resolve_consent_in(&dir, skill, version)
}

/// Install a registry skill onto `agent`, fail-closed unless `accept`.
pub async fn cmd_skill_registry_add(
    agent: &str,
    skill: &str,
    version: Option<&str>,
    accept: bool,
) -> Result<String> {
    let home = crate::cmd::agent::resolve_mur_home()?;
    let (dir, _idx) = skill_registry::fetch_and_load(&home, skill_registry::DEFAULT_REGISTRY)?;
    let consent = resolve_consent_in(&dir, skill, version)?;
    if consent.blocking && !accept {
        bail!(
            "refusing to install '{}': verify-on-install failed (hash={}, signature={}). Re-run with --yes to override.",
            consent.name, consent.hash, consent.signature.status
        );
    }
    if consent.needs_ack && !accept {
        bail!(
            "'{}' is {} (hash={}); re-run with --yes to install anyway.",
            consent.name, consent.signature.status, consent.hash
        );
    }
    let path = skill_registry::skill_yaml_path(&dir, skill, &consent.version);
    crate::cmd::agent::skill::cmd_skill_add(agent, &path.to_string_lossy())?;
    // Least privilege: registry skills land Sandboxed.
    let _ = mur_common::skill::local::set_trust_level(&home, &consent.name, TrustLevel::Sandboxed);
    Ok(format!("skills/{}", consent.name))
}

/// Search the registry for the agent flow (serializable view for the Hub).
pub fn registry_search_for_agent(mur_home: &Path, query: &str) -> Result<Vec<RegistrySkillEntryView>> {
    let (_dir, idx) = skill_registry::fetch_and_load(mur_home, skill_registry::DEFAULT_REGISTRY)?;
    Ok(skill_registry::search_registry(&idx, query)
        .into_iter()
        .map(|(name, e)| RegistrySkillEntryView {
            name: name.to_string(),
            description: e.description.clone(),
            publisher: e.publisher.clone(),
            category: e.category.clone(),
            latest: e.latest.clone(),
            signed_in_index: !e.content_sha256.is_empty(),
        })
        .collect())
}
```

Add to `mod.rs`: `pub mod skill_registry_add;`. (Confirm `ContentScanReport.findings` element type — adapt `f.to_string()` to the real shape, e.g. `format!("{}: {}", f.kind, f.detail)`.) Mark `resolve_consent`, `cmd_skill_registry_add`, `registry_search_for_agent`, and the view structs `#[allow(dead_code)]` only if `cargo clippy --all` reports them unused before the CLI/Hub wiring lands; the CLI task (3) will consume `cmd_skill_registry_add`.

- [ ] **Step 4: Run tests + lints**

Run: `ORT_STRATEGY=download cargo test -p mur-core --lib skill_registry_add` → PASS; `cargo clippy --all --no-deps -- -D warnings` clean; `cargo fmt --all`.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/skill_registry_add.rs mur-core/src/cmd/agent/mod.rs
git commit -m "feat(skill): per-agent registry-add (resolve+verify+install Sandboxed) + search view"
```

---

## Task 3: CLI verbs + dispatch (consent + confirm)

**Files:** Modify `mur-core/src/cli/agent.rs` (`AgentSkillAction`), `mur-core/src/dispatch.rs`.

**Interfaces:** Consumes Task 2 `cmd_skill_registry_add`, `resolve_consent`, `registry_search_for_agent`.

- [ ] **Step 1: Add the CLI variants** in `mur-core/src/cli/agent.rs` `enum AgentSkillAction` (after `AddUrl`):

```rust
    /// Install a skill from the registry onto the agent, by registry name
    /// (e.g. `mur agent skill registry-add rustsmith rust-testing`). Verifies
    /// the content hash + publisher signature before installing (fail-closed);
    /// installs at the Sandboxed trust level. Use --yes to accept an
    /// unsigned/unhashed skill or override a verify failure.
    RegistryAdd {
        /// Agent name
        name: String,
        /// Registry skill name
        skill: String,
        /// Specific version (defaults to latest)
        #[arg(long)]
        version: Option<String>,
        /// Accept despite verify warnings/failures
        #[arg(long)]
        yes: bool,
    },
    /// Search the skill registry (for installing onto this agent).
    Search {
        /// Agent name
        name: String,
        /// Search query
        query: String,
    },
```

- [ ] **Step 2: Wire dispatch** in `mur-core/src/dispatch.rs` (next to the `AddUrl` arm):

```rust
            AgentSkillAction::RegistryAdd { name, skill, version, yes } => {
                // Show consent before installing (skipped detail when --yes).
                if let Ok(c) = cmd::agent::skill_registry_add::resolve_consent(
                    &cmd::agent::resolve_mur_home()?, &skill, version.as_deref(),
                ) {
                    println!("Skill:     {} v{}", c.name, c.version);
                    println!("Publisher: {}", c.publisher);
                    println!("Signature: {}  Hash: {}", c.signature.status, c.hash);
                    if !c.mcp_requirements.is_empty() {
                        println!("Requires:  {}", c.mcp_requirements.join(", "));
                    }
                    for f in &c.findings { println!("  ! {f}"); }
                }
                let id = cmd::agent::skill_registry_add::cmd_skill_registry_add(
                    &name, &skill, version.as_deref(), yes,
                ).await?;
                println!("Installed {id} onto '{name}' (Sandboxed). Restart the agent to load it.");
            }
            AgentSkillAction::Search { name: _name, query } => {
                let rows = cmd::agent::skill_registry_add::registry_search_for_agent(
                    &cmd::agent::resolve_mur_home()?, &query,
                )?;
                if rows.is_empty() { println!("(no registry matches)"); }
                for r in rows {
                    let sig = if r.signed_in_index { "signed" } else { "unsigned" };
                    println!("  {:25} {:10} {} [{}] {}", r.name, r.category, r.publisher, r.latest, sig);
                }
            }
```

- [ ] **Step 3: Verify build + a parse test**

Run: `ORT_STRATEGY=download cargo build -p mur-core --bin mur` (compiles); add/adjust any existing CLI parse test that enumerates `AgentSkillAction`. Then `cargo clippy --all --no-deps -- -D warnings`; `cargo fmt --all`.
Expected: builds; `mur agent skill registry-add --help` and `... search --help` render.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cli/agent.rs mur-core/src/dispatch.rs
git commit -m "feat(cli): mur agent skill registry-add + search (consent + fail-closed)"
```

---

## Task 4: Hub backend commands

**Files:** Modify `mur-hub-gui/src-tauri/src/mcp_skills.rs`, `mur-hub-gui/src-tauri/src/lib.rs`.

**Interfaces:** Consumes Task 2 `registry_search_for_agent`, `resolve_consent`, `cmd_skill_registry_add`.

- [ ] **Step 1: Add the commands** in `mcp_skills.rs`:

```rust
use mur_core::cmd::agent::skill_registry_add::{
    ConsentInfo, RegistrySkillEntryView, cmd_skill_registry_add, registry_search_for_agent,
    resolve_consent,
};

/// Search the skill registry (Hub Browse).
#[tauri::command]
pub async fn agent_skill_registry_search(query: String) -> Result<Vec<RegistrySkillEntryView>, String> {
    let home = mur_core::cmd::agent::resolve_mur_home().map_err(|e| format!("{e:#}"))?;
    registry_search_for_agent(&home, &query).map_err(|e| format!("{e:#}"))
}

/// Build the consent screen for a registry skill (installs nothing).
#[tauri::command]
pub async fn agent_skill_registry_preview(skill: String, version: Option<String>) -> Result<ConsentInfo, String> {
    let home = mur_core::cmd::agent::resolve_mur_home().map_err(|e| format!("{e:#}"))?;
    resolve_consent(&home, &skill, version.as_deref()).map_err(|e| format!("{e:#}"))
}

/// Install a registry skill onto `name` (fail-closed unless accept).
#[tauri::command]
pub async fn agent_skill_registry_install(
    name: String, skill: String, version: Option<String>, accept: bool,
) -> Result<AgentDetail, String> {
    cmd_skill_registry_add(&name, &skill, version.as_deref(), accept)
        .await
        .map_err(|e| format!("{e:#}"))?;
    get_agent_detail(name)
}
```

Register all three in `lib.rs`'s `tauri::generate_handler![...]`.

- [ ] **Step 2: Verify build**

Run: `ORT_STRATEGY=download cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml` (stub `mur-hub-gui/ui/dist/index.html` if absent; don't commit). Then `cargo clippy --manifest-path mur-hub-gui/src-tauri/Cargo.toml -- -D warnings`; `cargo fmt --manifest-path mur-hub-gui/src-tauri/Cargo.toml --all`.

- [ ] **Step 3: Commit**

```bash
git add mur-hub-gui/src-tauri/src/mcp_skills.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(hub): skill registry search/preview/install Tauri commands"
```

---

## Task 5: Hub "Browse registry" UI + consent

**Files:** Modify `mur-hub-gui/ui/src/components/` (Skills tab + a `SkillRegistryModal.tsx`); i18n `en.ts`/`zh-TW.ts`.

**Interfaces:** Consumes `agent_skill_registry_search` → `RegistrySkillEntryView[]`, `agent_skill_registry_preview` → `ConsentInfo`, `agent_skill_registry_install` → `AgentDetail`.

- [ ] **Step 1: Add a "Browse registry" entry + modal**

In the Skills tab, add a "Browse registry" button opening `SkillRegistryModal`:

```tsx
// SkillRegistryModal.tsx (sketch — match existing modal style/state from SkillAddUrlModal.tsx)
const [results, setResults] = useState<RegistryView[] | null>(null);
const [consent, setConsent] = useState<ConsentInfo | null>(null);
const [accept, setAccept] = useState(false);

async function search() {
  setResults(await invoke<RegistryView[]>("agent_skill_registry_search", { query }));
}
async function preview(skill: string) {
  setConsent(await invoke<ConsentInfo>("agent_skill_registry_preview", { skill }));
  setAccept(false);
}
async function install() {
  if (!consent) return;
  const detail = await invoke<AgentDetail>("agent_skill_registry_install", {
    name: agentName, skill: consent.name, version: consent.version, accept,
  });
  onSaved(detail); onClose();
}
const canInstall = !!consent && (!consent.blocking && !consent.needs_ack || accept);
```

Consent view shows: `consent.publisher`, signature badge (`consent.signature.status` → ✓verified/⚠unsigned/✗invalid, with `key_fp`), `consent.hash` badge, requirements, findings list, and `consent.body` in a scrollable `<pre>`. Show the accept checkbox when `consent.blocking || consent.needs_ack`, gating Install.

- [ ] **Step 2: i18n** — add `skillreg.*` keys (title, search, install, unsigned/invalid/verified badges, "install anyway") to BOTH `en.ts` and `zh-TW.ts` (Traditional Chinese).

- [ ] **Step 3: Typecheck + build**

Run: `cd mur-hub-gui/ui && npx tsc --noEmit && npm run build` → tsc exit 0; vite succeeds.

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/ui/src/components/ mur-hub-gui/ui/src/i18n/en.ts mur-hub-gui/ui/src/i18n/zh-TW.ts
git commit -m "feat(hub): Browse registry view + verify-on-install consent"
```

- [ ] **Step 5: Live verify (manual)**

Point `DEFAULT_REGISTRY` at a local fixture git repo (or set an override env if added) laid out as `index.yaml` + `skills/<n>/versions/<v>.yaml` with: a clean signed+hashed skill (installs), a hash-tampered one (rejected fail-closed), an unsigned one (warns; installs only with `--yes`). Run `mur agent skill registry-add <scratch-agent> <name>` for each + check the Hub Browse flow.

---

## Self-Review

**Spec coverage:** per-agent `registry-add` → Task 2/3 ✓; verify-on-install fail-closed (content_sha256 + verify_manifest) → Task 1, gated in Task 2 ✓; Sandboxed install → Task 2 ✓; transparent consent (publisher/sig/hash/trust/MCP-reqs/findings/body) → `ConsentInfo` Task 2, surfaced CLI Task 3 + Hub Task 5 ✓; `mur agent skill search` → Task 3 ✓; Hub Browse → Task 4/5 ✓; degrade for unsigned/unhashed (warn + --yes) → `needs_ack` Task 1/2 ✓; rug-pull update re-verify → spec §3.6 is a **noted follow-on** (not a task here — `mur skill update` already exists; wiring `verify_skill_install` into it is a small separate change; flagged so the reviewer knows it's intentionally deferred). Companion registry-repo CI (populate content_sha256/publisher_signature) → spec §7, out of plan scope.

**Placeholder scan:** the three "confirm the real shape" notes (AgentIdentity constructor, ContentScanReport.findings element, existing CLI parse test) each name the exact file to check — not vague placeholders. No TBD/“handle errors”.

**Type consistency:** `VerifyOutcome{hash:HashStatus,signature:SignatureStatus}` + `is_blocking`/`needs_ack` (Task 1) consumed by `resolve_consent_in` (Task 2) → flattened into `ConsentInfo{signature:SigView,hash:String,blocking,needs_ack,...}` surfaced by CLI (Task 3) and Hub `agent_skill_registry_preview`→`ConsentInfo` (Task 4) → modal (Task 5). `cmd_skill_registry_add(agent,skill,version:Option<&str>,accept)` consistent across Task 2/3/4. `RegistrySkillEntryView` consistent Task 2→4→5.
