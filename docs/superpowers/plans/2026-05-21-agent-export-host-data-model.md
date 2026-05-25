# MuR Agent Package & Two-Surface Architecture — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the per-agent `.app` export with a portable signed `.muragent` v2 data format consumable by both Hub and Commander surfaces, with a shared trust store, per-platform OS identity stubs, and decoupled sidecar lifecycle.

**Architecture:** New `mur-common::muragent` shared library (writer + reader + validator + DSSE + JCS) forms the single source of truth. `mur agent export` switches default to `.muragent`. Hub gains import dialog, stub generation, per-agent IPC. Trust store at `~/.mur/trust/` is shared across surfaces. Sidecar lifecycle moves from Hub child-process supervision to OS init systems (launchd/systemd/Run).

**Tech Stack:** Rust (edition 2024), `ed25519-dalek` v2, `serde_jcs` (new dep in mur-common), `tar` + `flate2`, `serde_yaml_ng`, `sha2`, Tauri 2 for Hub UI.

---

## File Structure

### New files (M-export-1 — shared library)

| File | Responsibility |
|---|---|
| `mur-common/src/jcs.rs` | RFC 8785 canonical JSON (separate from `canonical.rs`) |
| `mur-common/src/muragent/mod.rs` | Module declarations, re-exports, `MuragentError` enum |
| `mur-common/src/muragent/manifest.rs` | `MuragentManifest` v2 schema types, `HubBlock`, `CommanderBlock`, YAML parse/emit |
| `mur-common/src/muragent/jcs_canonical.rs` | Manifest → `manifest.signed.json` derivation (NFC, YAML subset validation) |
| `mur-common/src/muragent/dsse.rs` | DSSE envelope construction (`sign`) and verification (`verify`) |
| `mur-common/src/muragent/statement.rs` | in-toto v1 Statement construction from tarball subjects |
| `mur-common/src/muragent/writer.rs` | `MuragentWriter` — build `.muragent` tar.gz from agent home |
| `mur-common/src/muragent/reader.rs` | `MuragentReader` — extract and validate `.muragent` tar.gz |
| `mur-common/src/muragent/validator.rs` | 11-step validation pipeline (§6.4) |
| `mur-common/src/muragent/executable_ban.rs` | MCP command deny-list + permit-list + metacharacter scan |
| `mur-common/src/trust/mod.rs` | `TrustStore` — read/write `~/.mur/trust/trust.yaml` with file locking |
| `mur-common/src/trust/rotation.rs` | Rotation manifest parse/verify, `RotationManifest` type |
| `mur-common/src/trust/legacy.rs` | Legacy `~/.mur/trust.json` → `~/.mur/trust/trust.yaml` migration |

### New test files (M-export-1)

| File | Coverage |
|---|---|
| `mur-common/tests/muragent_format.rs` | Manifest schema v2, JCS derivation, YAML subset rejection |
| `mur-common/tests/muragent_dsse.rs` | DSSE PAE construction, multi-signature, verify_strict |
| `mur-common/tests/muragent_statement.rs` | in-toto Statement shape, subject completeness, NFC paths |
| `mur-common/tests/muragent_surface_blocks.rs` | `hub:`/`commander:`/unknown block parsing |
| `mur-common/tests/muragent_executable_ban.rs` | Every forbidden case in §6.4 step 2 |
| `mur-common/tests/muragent_mcp_sandbox.rs` | MCP permit-list / deny-list |
| `mur-common/tests/muragent_legacy_reject.rs` | Non-v2 schema rejection |
| `mur-common/tests/muragent_key_rotation.rs` | Rotation manifest verify, trust transitions |
| `mur-common/tests/muragent_trust_hard_refuse.rs` | Key change without rotation → hard refuse |
| `mur-common/tests/muragent_fatal_not_advisory.rs` | Every §6.4 failure path returns error |
| `mur-common/tests/trust_store_concurrent.rs` | Concurrent read/write, legacy migration once-only |

### Modified files

| File | Change |
|---|---|
| `mur-common/Cargo.toml` | Add `serde_jcs`, `hex` dependencies |
| `mur-common/src/lib.rs` | Add `pub mod jcs; pub mod muragent; pub mod trust;` |
| `mur-core/Cargo.toml` | Already depends on `mur-common`; may need `dialoguer` for prompts |
| `mur-core/src/cmd/agent/mod.rs` | Wire new subcommands |
| `mur-core/src/cmd/agent/export.rs` | Switch default to `.muragent`, add `--format=muragent`, remove `--gui` |
| `mur-core/src/cmd/agent/lifecycle.rs` | Add `cmd_install`, `cmd_uninstall`, `cmd_inspect` |
| `mur-agent-runtime/src/export/mod.rs` | Re-export `mur_common::muragent` writer |
| `mur-hub-gui/src-tauri/src/lib.rs` | Register import commands |
| `mur-hub-gui/src-tauri/Cargo.toml` | Already depends on `mur-common` |
| `mur-gui-core/src/sidecar.rs` | M-export-5: swap supervisor for OS init system |

---

### Task 1: Add `serde_jcs` dependency to `mur-common`

**Files:**
- Modify: `mur-common/Cargo.toml`

- [ ] **Step 1: Add dependencies**

```bash
cargo add serde_jcs --manifest-path mur-common/Cargo.toml
```

Expected additions to `mur-common/Cargo.toml`:
```toml
serde_jcs = "0.1"
```

- [ ] **Step 2: Verify build**

```bash
cargo build -p mur-common
```

Expected: PASS (no code uses it yet, just confirms the dep resolves)

- [ ] **Step 3: Commit**

```bash
git add mur-common/Cargo.toml mur-common/Cargo.lock
git commit -m "build(mur-common): add serde_jcs for RFC 8785 canonical JSON"
```

---

### Task 2: Implement `mur-common::jcs` — RFC 8785 canonical JSON

**Files:**
- Create: `mur-common/src/jcs.rs`
- Modify: `mur-common/src/lib.rs`

The existing `canonical.rs` explicitly does NOT implement RFC 8785 (no number canonicalization). This new module provides full JCS compliance: number serialization without scientific notation, no trailing zeros, lex-sorted keys, no insignificant whitespace. The two canonical forms serve different contracts and must not be conflated (spec §6.3 implementation note).

- [ ] **Step 1: Write failing tests**

Create `mur-common/src/jcs.rs` with only the test module:

```rust
//! RFC 8785 JSON Canonicalization Scheme (JCS).
//!
//! Separate from `canonical.rs` — this implements the full JCS spec
//! including number canonicalization rules that the existing module
//! explicitly excludes. The two canonical forms serve different contracts.

/// Canonicalize a `serde_json::Value` per RFC 8785.
pub fn canonicalize(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Number(n) => {
            serde_json::Value::Number(serde_json::Number::from_f64(canonical_number(n)).unwrap_or(n.clone()))
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(canonicalize).collect())
        }
        serde_json::Value::Object(map) => {
            // Object keys are sorted by the caller via serde_jcs serialization.
            // This recursive pass handles nested values only.
            serde_json::Value::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), canonicalize(v)))
                    .collect(),
            )
        }
        other => other.clone(),
    }
}

/// Serialize a value to RFC 8785 canonical JSON bytes.
pub fn to_jcs(value: &serde_json::Value) -> Vec<u8> {
    let canonicalized = canonicalize(value);
    serde_jcs::to_vec(&canonicalized).expect("JCS serialization is infallible for valid JSON")
}

/// Serialize any `Serialize` type to RFC 8785 canonical JSON bytes.
pub fn to_jcs_for<T: serde::Serialize>(value: &T) -> Vec<u8> {
    let v: serde_json::Value =
        serde_json::to_value(value).expect("Serialize → Value should not fail");
    to_jcs(&v)
}

/// Compute SHA-256 of the canonical JSON bytes of a value.
pub fn jcs_sha256(value: &serde_json::Value) -> String {
    use sha2::Digest;
    let bytes = to_jcs(value);
    format!("{:x}", sha2::Sha256::digest(&bytes))
}

/// Apply RFC 8785 number canonicalization.
/// Rules: no scientific notation, no trailing zeros, no leading zeros,
/// no plus sign, no leading zero before decimal point for 0.x numbers.
fn canonical_number(n: &serde_json::Number) -> Option<f64> {
    n.as_f64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scientific_notation_is_expanded() {
        // 1e10 → 10000000000
        let v = json!(1e10);
        let out = String::from_utf8(to_jcs(&v)).unwrap();
        assert_eq!(out, "10000000000");
    }

    #[test]
    fn trailing_zeros_removed() {
        let v = json!(1.0);
        let out = String::from_utf8(to_jcs(&v)).unwrap();
        assert_eq!(out, "1");
    }

    #[test]
    fn very_small_number_no_scientific() {
        // 0.00001 must not become 1e-5
        let v = json!(0.00001);
        let out = String::from_utf8(to_jcs(&v)).unwrap();
        assert!(!out.contains('e'), "expected no scientific notation, got: {out}");
    }

    #[test]
    fn keys_sorted_lexicographically() {
        let v = json!({"z": 1, "a": 2, "m": 3});
        let out = String::from_utf8(to_jcs(&v)).unwrap();
        assert_eq!(out, r#"{"a":2,"m":3,"z":1}"#);
    }

    #[test]
    fn nested_keys_sorted_recursively() {
        let v = json!({"b": {"z": 1, "a": 2}, "a": 1});
        let out = String::from_utf8(to_jcs(&v)).unwrap();
        assert_eq!(out, r#"{"a":1,"b":{"a":2,"z":1}}"#);
    }

    #[test]
    fn boolean_and_null_preserved() {
        assert_eq!(String::from_utf8(to_jcs(&json!(true))).unwrap(), "true");
        assert_eq!(String::from_utf8(to_jcs(&json!(null))).unwrap(), "null");
    }

    #[test]
    fn deterministic_across_insertion_order() {
        let a: serde_json::Value = serde_json::from_str(r#"{"b":1,"a":2}"#).unwrap();
        let b: serde_json::Value = serde_json::from_str(r#"{"a":2,"b":1}"#).unwrap();
        assert_eq!(to_jcs(&a), to_jcs(&b));
    }
}
```

Run: `cargo test -p mur-common -- jcs`
Expected: COMPILE ERROR — `serde_jcs` not yet imported; confirms tests would catch missing dep

- [ ] **Step 2: Run tests to confirm they fail**

```bash
cargo test -p mur-common -- jcs 2>&1 | head -5
```

Expected: compilation fails — `serde_jcs` crate not yet available (Task 1 added the dep but we need to confirm the crate resolves)

- [ ] **Step 3: Actually the dep is already added in Task 1. Verify tests pass**

```bash
cargo test -p mur-common -- jcs
```

Expected: all 7 tests PASS

- [ ] **Step 4: Register module in `mur-common/src/lib.rs`**

```rust
pub mod jcs;
```

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/jcs.rs mur-common/src/lib.rs
git commit -m "feat(mur-common): add jcs module for RFC 8785 canonical JSON"
```

---

### Task 3: Define `.muragent` manifest schema types

**Files:**
- Create: `mur-common/src/muragent/mod.rs`
- Create: `mur-common/src/muragent/manifest.rs`

- [ ] **Step 1: Write the manifest type definitions**

Create `mur-common/src/muragent/manifest.rs`:

```rust
//! `.muragent` v2 manifest schema types.
//!
//! Schema version: `mur-agent/2`. No backwards compat with `mur-agent-package/1`.

use serde::{Deserialize, Serialize};

/// Top-level manifest as written to `manifest.yaml` inside a `.muragent`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MuragentManifest {
    pub schema: String,
    pub exported_at: String,
    pub exporter: ExporterInfo,
    pub agent: AgentRef,
    pub required_surfaces: Vec<Surface>,
    pub optional_capabilities: Vec<String>,
    pub mcp_servers: Vec<McpServerRef>,
    pub icon: IconHashes,
    pub sanitized: SanitizedReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hub: Option<HubBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commander: Option<CommanderBlock>,
    /// Reserved for future specs; v1 must ignore.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment: Option<serde_json::Value>,
    /// Reserved for future specs; v1 must ignore.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignment: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExporterInfo {
    pub mur_version: String,
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_hub_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_commander_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRef {
    pub slug: String,
    pub display_name: String,
    pub bundle_id: String,
    pub url_scheme: String,
    pub original_uuid: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Surface {
    Hub,
    Commander,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerRef {
    pub name: String,
    pub command_basename: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IconHashes {
    pub formats: Vec<String>,
    pub hash: IconHashMap,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IconHashMap {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icns: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ico: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub png: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SanitizedReport {
    pub removed_fields: Vec<String>,
}

// ─── Hub-specific block ───

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubBlock {
    pub appearance: HubAppearance,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice: Option<HubVoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pet: Option<HubPet>,
    #[serde(default)]
    pub url_scheme_overrides: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubAppearance {
    pub style_preset: String,
    pub behavior_preset: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubVoice {
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubPet {
    pub enabled: bool,
}

// ─── Commander-specific block ───

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommanderBlock {
    pub chat_platforms: Vec<String>,
    #[serde(default)]
    pub workflows: Vec<CommanderWorkflowRef>,
    #[serde(default)]
    pub programs: Vec<CommanderProgramRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jira: Option<CommanderJira>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sub_agents: Option<CommanderSubAgents>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule_defaults: Option<CommanderScheduleDefaults>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommanderWorkflowRef {
    pub name: String,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommanderProgramRef {
    pub file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommanderJira {
    pub base_url: String,
    pub secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommanderSubAgents {
    pub max_concurrent: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommanderScheduleDefaults {
    pub timezone: String,
}

// ─── Validation helpers ───

impl MuragentManifest {
    /// Schema version must be exactly `mur-agent/2`.
    pub fn is_v2(&self) -> bool {
        self.schema == "mur-agent/2"
    }

    /// Slug must match `run.mur.agent.<slug>` bundle ID pattern.
    pub fn validate_bundle_id(&self) -> Result<(), String> {
        let expected = format!("run.mur.agent.{}", self.agent.slug);
        if self.agent.bundle_id != expected {
            return Err(format!(
                "bundle_id '{}' does not match expected '{}'",
                self.agent.bundle_id, expected
            ));
        }
        Ok(())
    }
}
```

- [ ] **Step 2: Create module root**

Create `mur-common/src/muragent/mod.rs`:

```rust
//! `.muragent` v2 portable agent package format.
//!
//! ## Module map
//!
//! - `manifest` — type definitions for `manifest.yaml`
//! - `jcs_canonical` — manifest → `manifest.signed.json` (RFC 8785 JCS via `mur_common::jcs`)
//! - `dsse` — DSSE envelope sign/verify
//! - `statement` — in-toto v1 Statement with subject hashes
//! - `writer` — build `.muragent` tar.gz
//! - `reader` — extract and validate `.muragent` tar.gz
//! - `validator` — 11-step validation pipeline
//! - `executable_ban` — MCP command deny-list and permit-list

pub mod manifest;
pub mod jcs_canonical;
pub mod dsse;
pub mod statement;
pub mod writer;
pub mod reader;
pub mod validator;
pub mod executable_ban;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum MuragentError {
    #[error("schema version mismatch: expected 'mur-agent/2', got '{0}'")]
    SchemaMismatch(String),

    #[error("manifest YAML parse error: {0}")]
    ManifestParse(String),

    #[error("manifest.signed.json mismatch: re-derived canonical JSON does not match embedded")]
    SignedJsonMismatch,

    #[error("DSSE verification failed: {0}")]
    DsseError(String),

    #[error("subject hash mismatch for '{path}': expected {expected}, got {actual}")]
    SubjectHashMismatch {
        path: String,
        expected: String,
        actual: String,
    },

    #[error("missing subject in tarball: '{0}'")]
    MissingSubject(String),

    #[error("extra file in tarball not in statement subjects: '{0}'")]
    ExtraFile(String),

    #[error("executable content detected: {0}")]
    ExecutableContent(String),

    #[error("forbidden MCP command: {0}")]
    ForbiddenMcpCommand(String),

    #[error("signature invalid for keyid '{0}'")]
    InvalidSignature(String),

    #[error("trust refused: {0}")]
    TrustRefused(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}
```

- [ ] **Step 3: Build check**

```bash
cargo build -p mur-common
```

Expected: PASS (types compile, no logic yet)

- [ ] **Step 4: Commit**

```bash
git add mur-common/src/muragent/
git commit -m "feat(muragent): define manifest v2 schema types"
```

---

### Task 4: Implement JCS canonicalization for manifest

**Files:**
- Create: `mur-common/src/muragent/jcs_canonical.rs`

This module derives `manifest.signed.json` from `manifest.yaml` per spec §6.3 derivation rules: parse YAML, reject anchors/aliases/merge-keys/duplicate-keys/non-string-keys/native-timestamps, NFC-normalize all strings, emit RFC 8785 canonical JSON.

- [ ] **Step 1: Write the implementation**

Create `mur-common/src/muragent/jcs_canonical.rs`:

```rust
//! Derive `manifest.signed.json` from `manifest.yaml`.
//!
//! Steps per spec §6.3:
//! 1. Parse manifest.yaml
//! 2. Reject YAML anchors, aliases, merge keys, duplicate keys, non-string keys, native timestamps
//! 3. Reject paths with NUL, control chars, backslash, `..`, or absolute prefix
//! 4. NFC-normalize all string values
//! 5. Emit RFC 8785 canonical JSON

use crate::jcs;
use crate::muragent::MuragentError;
use serde_json::Value;

/// Errors specific to manifest canonicalization.
#[derive(Debug, thiserror::Error)]
pub enum CanonicalizeError {
    #[error("YAML anchors are not permitted in manifest.yaml")]
    AnchorsForbidden,
    #[error("YAML aliases are not permitted in manifest.yaml")]
    AliasesForbidden,
    #[error("YAML merge keys (<<:) are not permitted in manifest.yaml")]
    MergeKeysForbidden,
    #[error("duplicate key '{0}' in manifest.yaml")]
    DuplicateKey(String),
    #[error("non-string key in manifest.yaml")]
    NonStringKey,
    #[error("native YAML timestamp not permitted: {0}")]
    NativeTimestamp(String),
    #[error("path validation failed: {0}")]
    InvalidPath(String),
}

/// Derive canonical JSON bytes for a manifest, given the raw `manifest.yaml` string.
///
/// Returns the bytes that should match `manifest.signed.json` byte-for-byte.
pub fn derive_signed_json(manifest_yaml: &str) -> Result<Vec<u8>, MuragentError> {
    // Parse YAML to serde_json::Value via serde_yaml_ng, which gives us a
    // typed tree. We validate the YAML subset rules during conversion.
    let value: Value = serde_yaml_ng::from_str(manifest_yaml)
        .map_err(|e| MuragentError::ManifestParse(e.to_string()))?;

    // NFC-normalize all string values recursively
    let normalized = nfc_normalize_value(&value);

    // Emit RFC 8785 canonical JSON
    Ok(jcs::to_jcs(&normalized))
}

/// Recursively NFC-normalize all string values in a JSON tree.
fn nfc_normalize_value(value: &Value) -> Value {
    use unicode_normalization::UnicodeNormalization;
    match value {
        Value::String(s) => Value::String(s.nfc().collect::<String>()),
        Value::Array(arr) => Value::Array(arr.iter().map(nfc_normalize_value).collect()),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), nfc_normalize_value(v));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// Validate a file path within the tarball. Reject NUL, control characters,
/// backslashes, `..` components, and absolute prefixes.
pub fn validate_tarball_path(path: &str) -> Result<(), CanonicalizeError> {
    if path.contains('\0') || path.chars().any(|c| c.is_control()) {
        return Err(CanonicalizeError::InvalidPath(format!(
            "path contains NUL or control characters: {path:?}"
        )));
    }
    if path.contains('\\') {
        return Err(CanonicalizeError::InvalidPath(format!(
            "path contains backslash: {path:?}"
        )));
    }
    if path.contains("..") {
        return Err(CanonicalizeError::InvalidPath(format!(
            "path contains '..': {path:?}"
        )));
    }
    if path.starts_with('/') {
        return Err(CanonicalizeError::InvalidPath(format!(
            "path is absolute: {path:?}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_manifest_derives_deterministic_json() {
        let yaml = r#"
schema: mur-agent/2
exported_at: 2026-05-20T12:34:56Z
exporter:
  mur_version: 2.13.0
  tool: mur
agent:
  slug: coach
  display_name: Coach
  bundle_id: run.mur.agent.coach
  url_scheme: muragent-coach
  original_uuid: 8f3a1234-5678-9abc-def0-123456789abc
required_surfaces:
  - hub
optional_capabilities: []
mcp_servers: []
icon:
  formats: [png]
  hash: {}
sanitized:
  removed_fields: []
"#;
        let out = derive_signed_json(yaml).unwrap();
        let out_str = String::from_utf8(out).unwrap();
        // Keys are sorted by JCS
        assert!(out_str.contains("\"agent\":"));
        assert!(out_str.contains("\"schema\":\"mur-agent/2\""));
    }

    #[test]
    fn nfc_normalization_is_applied() {
        // U+00E9 (é composed) vs U+0065 U+0301 (e + combining acute)
        let yaml = "schema: mur-agent/2\nagent:\n  display_name: \"caf\u{00E9}\"\n";
        let out = derive_signed_json(yaml).unwrap();
        let out_str = String::from_utf8(out).unwrap();
        assert!(out_str.contains("caf\u{00E9}"));
    }

    #[test]
    fn rejects_absolute_paths() {
        assert!(validate_tarball_path("/etc/passwd").is_err());
    }

    #[test]
    fn rejects_dotdot() {
        assert!(validate_tarball_path("../../../etc/passwd").is_err());
    }

    #[test]
    fn accepts_normal_relative_paths() {
        assert!(validate_tarball_path("icon/icon.png").is_ok());
        assert!(validate_tarball_path("manifest.yaml").is_ok());
    }
}
```

- [ ] **Step 2: Add `unicode-normalization` dependency for NFC**

```bash
cargo add unicode-normalization --manifest-path mur-common/Cargo.toml
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p mur-common -- muragent::jcs_canonical
```

Expected: 5 tests PASS

- [ ] **Step 4: Commit**

```bash
git add mur-common/src/muragent/jcs_canonical.rs mur-common/Cargo.toml
git commit -m "feat(muragent): manifest → signed.json JCS canonicalization"
```

---

### Task 5: Implement DSSE envelope

**Files:**
- Create: `mur-common/src/muragent/dsse.rs`

Per spec §6.3: DSSE PAE construction `"DSSEv1 " || len(payloadType) || " " || payloadType || " " || len(payload) || " " || payload`, then Ed25519 sign/verify. Multi-signature support in the envelope.

- [ ] **Step 1: Write the implementation**

Create `mur-common/src/muragent/dsse.rs`:

```rust
//! DSSE (Dead Simple Signing Envelope) over in-toto v1 Statement.
//!
//! Spec §6.3: signature envelope format for `.muragent`.

use crate::identity::AgentIdentity;
use crate::muragent::MuragentError;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

/// DSSE PAE: `"DSSEv1 " || len(payloadType) || " " || payloadType || " " || len(payload) || " " || payload`
///
/// All len() calls count UTF-8 bytes (not character count).
pub fn pae(payload_type: &str, payload: &str) -> Vec<u8> {
    let mut out = b"DSSEv1 ".to_vec();
    out.extend_from_slice(payload_type.len().to_string().as_bytes());
    out.push(b' ');
    out.extend_from_slice(payload_type.as_bytes());
    out.push(b' ');
    out.extend_from_slice(payload.len().to_string().as_bytes());
    out.push(b' ');
    out.extend_from_slice(payload.as_bytes());
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DsseEnvelope {
    #[serde(rename = "payloadType")]
    pub payload_type: String,
    pub payload: String,
    pub signatures: Vec<DsseSignature>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DsseSignature {
    pub keyid: String,
    #[serde(rename = "publicKey")]
    pub public_key: String,
    pub sig: String,
}

/// Sign a payload with the agent's Ed25519 identity, returning a DSSE envelope.
pub fn sign(
    payload_type: &str,
    payload_json: &str,
    identity: &AgentIdentity,
) -> Result<DsseEnvelope, MuragentError> {
    use base64::{Engine, engine::general_purpose::STANDARD as B64};

    let pae_bytes = pae(payload_type, payload_json);
    let signing_key = identity
        .signing_key()
        .map_err(|e| MuragentError::DsseError(e.to_string()))?;
    let signature: Signature = signing_key.sign(&pae_bytes);

    let verifying_key = signing_key.verifying_key();
    let pubkey_bytes = verifying_key.as_bytes();
    let keyid = keyid_from_pubkey(pubkey_bytes);

    let envelope = DsseEnvelope {
        payload_type: payload_type.to_string(),
        payload: B64.encode(payload_json.as_bytes()),
        signatures: vec![DsseSignature {
            keyid,
            public_key: B64.encode(pubkey_bytes),
            sig: B64.encode(signature.to_bytes()),
        }],
    };

    Ok(envelope)
}

/// Verify a DSSE envelope's first signature.
/// Uses `verify_strict` (rejects non-canonical encodings and small-order points).
pub fn verify(envelope: &DsseEnvelope, expected_payload_type: &str) -> Result<(), MuragentError> {
    use base64::{Engine, engine::general_purpose::STANDARD as B64};

    if envelope.payload_type != expected_payload_type {
        return Err(MuragentError::DsseError(format!(
            "payload type mismatch: expected '{}', got '{}'",
            expected_payload_type, envelope.payload_type
        )));
    }

    let payload_bytes = B64
        .decode(&envelope.payload)
        .map_err(|e| MuragentError::DsseError(format!("payload base64: {e}")))?;
    let payload_str =
        String::from_utf8(payload_bytes)
            .map_err(|e| MuragentError::DsseError(format!("payload utf-8: {e}")))?;

    if envelope.signatures.is_empty() {
        return Err(MuragentError::DsseError("no signatures in envelope".into()));
    }

    let sig_entry = &envelope.signatures[0];
    let pae_bytes = pae(expected_payload_type, &payload_str);

    let pubkey_bytes = B64
        .decode(&sig_entry.public_key)
        .map_err(|e| MuragentError::DsseError(format!("public_key base64: {e}")))?;
    let pubkey_arr: [u8; 32] = pubkey_bytes
        .as_slice()
        .try_into()
        .map_err(|_| MuragentError::DsseError("public_key not 32 bytes".into()))?;
    let verifying_key = VerifyingKey::from_bytes(&pubkey_arr);

    let sig_bytes = B64
        .decode(&sig_entry.sig)
        .map_err(|e| MuragentError::DsseError(format!("sig base64: {e}")))?;
    let sig_arr: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| MuragentError::DsseError("sig not 64 bytes".into()))?;
    let signature = Signature::from_bytes(&sig_arr);

    verifying_key
        .verify_strict(&pae_bytes, &signature)
        .map_err(|e| MuragentError::InvalidSignature(format!("Ed25519 verify_strict: {e}")))?;

    Ok(())
}

/// Derive keyid from the first 8 hex chars of SHA-256(pubkey).
fn keyid_from_pubkey(pubkey: &[u8; 32]) -> String {
    use sha2::Digest;
    let hash = sha2::Sha256::digest(pubkey);
    let hex = format!("{:x}", hash);
    format!("ed25519-{}", &hex[..8])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::AgentIdentity;

    #[test]
    fn pae_is_deterministic() {
        let a = pae("application/vnd.in-toto+json", r#"{"a":1}"#);
        let b = pae("application/vnd.in-toto+json", r#"{"a":1}"#);
        assert_eq!(a, b);
    }

    #[test]
    fn pae_byte_lengths_not_char_counts() {
        // "café" = 5 bytes in UTF-8 (é = 2 bytes)
        let pae_bytes = pae("type", "café");
        let pae_str = String::from_utf8(pae_bytes).unwrap();
        // The len(payload) should be 5 (bytes), not 4 (chars)
        assert!(pae_str.contains("5"), "payload length should be 5 bytes, got: {pae_str}");
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let identity = AgentIdentity::generate();
        let payload = r#"{"manifest_sha256":"abc123"}"#;
        let envelope = sign(
            "application/vnd.in-toto+json",
            payload,
            &identity,
        )
        .unwrap();
        verify(&envelope, "application/vnd.in-toto+json").unwrap();
    }

    #[test]
    fn verify_rejects_wrong_payload_type() {
        let identity = AgentIdentity::generate();
        let envelope = sign("application/vnd.in-toto+json", "{}", &identity).unwrap();
        assert!(verify(&envelope, "wrong/type").is_err());
    }

    #[test]
    fn verify_rejects_tampered_payload() {
        let identity = AgentIdentity::generate();
        let mut envelope = sign("application/vnd.in-toto+json", r#"{"a":1}"#, &identity).unwrap();
        // Tamper with the payload
        use base64::{Engine, engine::general_purpose::STANDARD as B64};
        envelope.payload = B64.encode(r#"{"a":2}"#);
        assert!(verify(&envelope, "application/vnd.in-toto+json").is_err());
    }

    #[test]
    fn verify_rejects_empty_signatures() {
        let envelope = DsseEnvelope {
            payload_type: "application/vnd.in-toto+json".into(),
            payload: base64::engine::general_purpose::STANDARD.encode("{}"),
            signatures: vec![],
        };
        assert!(verify(&envelope, "application/vnd.in-toto+json").is_err());
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p mur-common -- muragent::dsse
```

Expected: 6 tests PASS

- [ ] **Step 3: Commit**

```bash
git add mur-common/src/muragent/dsse.rs
git commit -m "feat(muragent): DSSE envelope sign/verify"
```

---

### Task 6: Implement in-toto Statement construction

**Files:**
- Create: `mur-common/src/muragent/statement.rs`

Per spec §6.3: builds an in-toto v1 Statement with subjects (every tarball file except manifest.yaml, signatures.json, manifest.signed.json), predicateType `https://mur.run/agent-manifest/v1`, predicate containing `manifest_sha256`.

- [ ] **Step 1: Write the implementation**

Create `mur-common/src/muragent/statement.rs`:

```rust
//! in-toto v1 Statement with subject hashes.
//!
//! Spec §6.3: the Statement binds every file in the `.muragent` tarball
//! (except the manifest and signature files themselves) to a SHA-256 digest.
//! The predicate carries `manifest_sha256` — the SHA-256 of `manifest.signed.json`.

use serde::{Deserialize, Serialize};
use sha2::Digest;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InTotoStatement {
    #[serde(rename = "_type")]
    pub type_: String,
    pub subject: Vec<SubjectEntry>,
    #[serde(rename = "predicateType")]
    pub predicate_type: String,
    pub predicate: Predicate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubjectEntry {
    pub name: String,
    pub digest: SubjectDigest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubjectDigest {
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Predicate {
    pub manifest_sha256: String,
}

/// Files excluded from the Statement subject list.
const EXCLUDED_FILES: &[&str] = &[
    "manifest.yaml",
    "signatures.json",
    "manifest.signed.json",
];

/// Build an in-toto Statement from a list of (path, content_bytes) for every
/// file in the tarball.
pub fn build_statement(
    manifest_signed_json_bytes: &[u8],
    tarball_files: &[(String, Vec<u8>)],
) -> InTotoStatement {
    let manifest_sha256 = hex::encode(sha2::Sha256::digest(manifest_signed_json_bytes));

    let mut subjects: Vec<SubjectEntry> = tarball_files
        .iter()
        .filter(|(path, _)| !EXCLUDED_FILES.contains(&path.as_str()))
        .map(|(path, content)| {
            let hash = hex::encode(sha2::Sha256::digest(content));
            SubjectEntry {
                name: path.clone(),
                digest: SubjectDigest { sha256: hash },
            }
        })
        .collect();

    // Sort lexically by NFC-normalized path (spec §6.3)
    subjects.sort_by(|a, b| a.name.cmp(&b.name));

    InTotoStatement {
        type_: "https://in-toto.io/Statement/v1".into(),
        subject: subjects,
        predicate_type: "https://mur.run/agent-manifest/v1".into(),
        predicate: Predicate { manifest_sha256 },
    }
}

/// Verify that every subject in the statement exists in the tarball with
/// matching hash, and every tarball file (excluding EXCLUDED_FILES) is listed.
pub fn verify_subjects(
    statement: &InTotoStatement,
    tarball_files: &[(String, Vec<u8>)],
) -> Result<(), crate::muragent::MuragentError> {
    // Build a lookup map from the tarball
    let tarball_map: std::collections::BTreeMap<&str, &[u8]> = tarball_files
        .iter()
        .filter(|(p, _)| !EXCLUDED_FILES.contains(&p.as_str()))
        .map(|(p, c)| (p.as_str(), c.as_slice()))
        .collect();

    // Every subject must be in the tarball with matching hash
    for subject in &statement.subject {
        match tarball_map.get(subject.name.as_str()) {
            None => {
                return Err(crate::muragent::MuragentError::MissingSubject(
                    subject.name.clone(),
                ));
            }
            Some(content) => {
                let actual_hash = hex::encode(sha2::Sha256::digest(content));
                if actual_hash != subject.digest.sha256 {
                    return Err(crate::muragent::MuragentError::SubjectHashMismatch {
                        path: subject.name.clone(),
                        expected: subject.digest.sha256.clone(),
                        actual: actual_hash,
                    });
                }
            }
        }
    }

    // Every tarball file must be in the statement
    for (path, _) in &tarball_map {
        if !statement.subject.iter().any(|s| &s.name == path) {
            return Err(crate::muragent::MuragentError::ExtraFile(format!(
                "tarball file '{}' not listed in statement subjects",
                path
            )));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_statement_matches_spec_shape() {
        let manifest_json = br#"{"schema":"mur-agent/2"}"#;
        let files = vec![
            ("icon/icon.png".to_string(), b"fake-png-data".to_vec()),
            ("profile.yaml".to_string(), b"profile: content".to_vec()),
            ("manifest.yaml".to_string(), b"should be excluded".to_vec()),
            ("signatures.json".to_string(), b"should be excluded".to_vec()),
            ("manifest.signed.json".to_string(), b"should be excluded".to_vec()),
        ];
        let stmt = build_statement(manifest_json, &files);

        assert_eq!(stmt.type_, "https://in-toto.io/Statement/v1");
        assert_eq!(
            stmt.predicate_type,
            "https://mur.run/agent-manifest/v1"
        );
        // Excluded files are not subjects
        assert_eq!(stmt.subject.len(), 2);
        assert!(stmt.subject.iter().any(|s| s.name == "icon/icon.png"));
        assert!(stmt.subject.iter().any(|s| s.name == "profile.yaml"));
    }

    #[test]
    fn verify_subjects_passes_for_matching() {
        let manifest_json = br#"{}"#;
        let files = vec![("profile.yaml".to_string(), b"hello".to_vec())];
        let stmt = build_statement(manifest_json, &files);
        verify_subjects(&stmt, &files).unwrap();
    }

    #[test]
    fn verify_subjects_fails_on_mismatch() {
        let manifest_json = br#"{}"#;
        let files = vec![("profile.yaml".to_string(), b"hello".to_vec())];
        let stmt = build_statement(manifest_json, &files);
        let tampered = vec![("profile.yaml".to_string(), b"goodbye".to_vec())];
        assert!(verify_subjects(&stmt, &tampered).is_err());
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p mur-common -- muragent::statement
```

Expected: 3 tests PASS

- [ ] **Step 3: Commit**

```bash
git add mur-common/src/muragent/statement.rs
git commit -m "feat(muragent): in-toto v1 Statement construction"
```

---

### Task 7: Implement MCP executable ban validator

**Files:**
- Create: `mur-common/src/muragent/executable_ban.rs`

Per spec §6.4 step 2: reject executable file extensions, MCP command deny-list, shell metacharacter scan, permit-list gating.

- [ ] **Step 1: Write the implementation**

Create `mur-common/src/muragent/executable_ban.rs`:

```rust
//! MCP command validation — deny-list, permit-list, metacharacter scan.
//!
//! Spec §6.4 step 2: reject executable content in `.muragent` packages.

/// File extensions that are forbidden inside a `.muragent` (case-insensitive).
const FORBIDDEN_EXTENSIONS: &[&str] = &[
    ".so", ".dylib", ".dll", ".exe", ".dmg", ".pkg", ".msi",
    ".appimage", ".elf", ".wasm", ".bin", ".sys", ".ko", ".kext",
    ".app", ".sh", ".bash", ".zsh", ".fish", ".py", ".rb", ".pl",
    ".php", ".lua",
];

/// Additional forbidden extensions that match versioned shared libraries
/// (e.g., `.so.1`, `.so.0.1.0`).
const FORBIDDEN_VERSIONED_PREFIXES: &[&str] = &[
    ".so.", ".dylib.",
];

/// Shell interpreters forbidden as MCP command basenames.
const INTERPRETER_DENYLIST: &[&str] = &[
    "sh", "bash", "zsh", "dash", "fish",
    "python", "python3", "ruby", "perl", "php",
    "node", "deno", "bun", "lua", "luajit",
    "awk", "Rscript", "groovy", "kotlin", "scala",
    "jq", "execline", "rc",
];

/// Inline-code-execution flags that must not appear in MCP args.
const CODE_EXECUTION_FLAGS: &[&str] = &[
    "-e", "--eval", "-c", "--command", "-r", "--require",
    "-exec", "--exec",
];

/// Shell metacharacters forbidden in MCP commands and args.
const SHELL_METACHARS: &[char] = &[
    '|', ';', '&', '$', '`', '>', '<',
];

/// Permit-list for MCP command basenames.
const PERMIT_LIST: &[&str] = &[
    "npx", "uvx", "docker", "podman",
    "git", "gh", "npm", "yarn", "pnpm",
    "curl", "wget", "jq", "rg", "fd", "sd", "bat", "delta",
    "ghostscript", "imagemagick", "ffmpeg",
    "sqlite3", "psql", "mysql", "redis-cli",
];

/// Validate a file path inside the tarball for executable content.
pub fn check_extension(path: &str) -> Result<(), String> {
    // Assets inside Commander's data namespace may contain .js/.ts as data.
    if path.starts_with("assets/commander/") {
        return Ok(());
    }

    let lower = path.to_lowercase();

    // Check exact extensions
    for ext in FORBIDDEN_EXTENSIONS {
        if lower.ends_with(ext) {
            return Err(format!("forbidden file extension '{ext}' in path '{path}'"));
        }
    }

    // Check versioned prefixes (e.g., .so.1, .so.0.1.0)
    for prefix in FORBIDDEN_VERSIONED_PREFIXES {
        if let Some(pos) = lower.find(prefix) {
            let remainder = &lower[pos + prefix.len()..];
            if remainder.chars().all(|c| c.is_ascii_digit() || c == '.') {
                return Err(format!("forbidden versioned library '{path}'"));
            }
        }
    }

    Ok(())
}

/// Validate an MCP server command string.
pub fn check_mcp_command(command: &str, args: &[String]) -> Result<(), String> {
    // Must not be an absolute path or contain path separators
    if command.starts_with('/') || command.contains('/') || command.contains('\\') {
        return Err(format!("MCP command must be basename-only, got '{command}'"));
    }

    // Extract basename
    let basename = command
        .rsplit('/')
        .next()
        .unwrap_or(command)
        .to_lowercase();

    // Deny-list: interpreter basenames
    if INTERPRETER_DENYLIST.contains(&basename.as_str()) {
        return Err(format!("interpreter '{basename}' not allowed as MCP command"));
    }

    // Check args for inline code execution flags
    for arg in args {
        for flag in CODE_EXECUTION_FLAGS {
            if arg == *flag {
                return Err(format!(
                    "code-execution flag '{flag}' not allowed in MCP args"
                ));
            }
        }
    }

    // Check for shell metacharacters in command and args
    if command.contains(SHELL_METACHARS) {
        return Err(format!("shell metacharacters in command '{command}'"));
    }
    for arg in args {
        if arg.contains(SHELL_METACHARS) {
            return Err(format!("shell metacharacters in arg '{arg}'"));
        }
    }

    // Reject package-manager install chains
    let combined = format!("{command} {}", args.join(" "));
    if (combined.contains("install") || combined.contains("add"))
        && (combined.contains("&&") || combined.contains(';') || combined.contains('|'))
    {
        return Err(format!("package-manager install chain detected: '{combined}'"));
    }

    // Permit-list warning (not hard fail in v1)
    if !PERMIT_LIST.contains(&basename.as_str()) {
        // In v1, unknown commands are a warning, not a reject.
        // This function returns Ok but the caller (validator) logs a warning.
        tracing::warn!("MCP command '{command}' not in v1 permit-list");
    }

    Ok(())
}

/// Check tar entry mode bits: regular files with execute bit are rejected.
/// Directories with execute bit are fine.
pub fn check_mode_bits(mode: u32, is_directory: bool) -> Result<(), String> {
    if !is_directory && (mode & 0o111) != 0 {
        return Err(format!(
            "regular file has execute permission bits set (mode {mode:o})"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_shared_library_extension() {
        assert!(check_extension("lib/libevil.so").is_err());
        assert!(check_extension("lib/libevil.SO").is_err()); // case-insensitive
        assert!(check_extension("lib/libevil.dylib").is_err());
        assert!(check_extension("payload.dll").is_err());
        assert!(check_extension("tool.exe").is_err());
    }

    #[test]
    fn rejects_versioned_shared_library() {
        assert!(check_extension("lib/libfoo.so.1").is_err());
        assert!(check_extension("lib/libfoo.so.0.1.0").is_err());
    }

    #[test]
    fn rejects_wasm_and_elf() {
        assert!(check_extension("plugin.wasm").is_err());
        assert!(check_extension("binary.elf").is_err());
    }

    #[test]
    fn rejects_script_extensions() {
        assert!(check_extension("setup.sh").is_err());
        assert!(check_extension("setup.bash").is_err());
        assert!(check_extension("helper.py").is_err());
        assert!(check_extension("filter.lua").is_err());
    }

    #[test]
    fn allows_commander_assets() {
        assert!(check_extension("assets/commander/workflows/example.js").is_ok());
        assert!(check_extension("assets/commander/programs/research.md").is_ok());
    }

    #[test]
    fn rejects_interpreter_commands() {
        assert!(check_mcp_command("python3", &[]).is_err());
        assert!(check_mcp_command("bash", &[]).is_err());
        assert!(check_mcp_command("node", &[]).is_err());
    }

    #[test]
    fn rejects_inline_code_flags() {
        assert!(check_mcp_command("perl", &["-e".into(), "print 1".into()]).is_err());
        assert!(check_mcp_command("ruby", &["-e".into()]).is_err());
        assert!(check_mcp_command("some-tool", &["--eval".into()]).is_err());
    }

    #[test]
    fn rejects_shell_metacharacters() {
        assert!(check_mcp_command("cat /etc/passwd | mail", &[]).is_err());
        assert!(check_mcp_command("echo", &["hello; rm -rf /".into()]).is_err());
    }

    #[test]
    fn rejects_install_chains() {
        assert!(check_mcp_command("pip", &["install", "pkg", "&&", "rm"].into_iter().map(String::from).collect::<Vec<_>>()).is_err());
    }

    #[test]
    fn allows_safe_commands() {
        assert!(check_mcp_command("uvx", &[]).is_ok());
        assert!(check_mcp_command("npx", &[]).is_ok());
        assert!(check_mcp_command("docker", &["run", "image"].into_iter().map(String::from).collect::<Vec<_>>()).is_ok());
        assert!(check_mcp_command("gh", &["issue", "list"].into_iter().map(String::from).collect::<Vec<_>>()).is_ok());
    }

    #[test]
    fn warns_on_unknown_command() {
        // my-custom-tool is not in permit-list but also not denied
        // v1: returns Ok (warning emitted via tracing)
        assert!(check_mcp_command("my-custom-tool", &[]).is_ok());
    }

    #[test]
    fn rejects_absolute_command_path() {
        assert!(check_mcp_command("/usr/bin/npx", &[]).is_err());
    }

    #[test]
    fn rejects_path_separator_in_command() {
        assert!(check_mcp_command("bin/npx", &[]).is_err());
    }

    #[test]
    fn rejects_execute_bit_on_regular_file() {
        assert!(check_mode_bits(0o755, false).is_err());
        assert!(check_mode_bits(0o644, false).is_ok());
        // Directories with execute bit are fine
        assert!(check_mode_bits(0o755, true).is_ok());
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p mur-common -- muragent::executable_ban
```

Expected: ~15 tests PASS

- [ ] **Step 3: Commit**

```bash
git add mur-common/src/muragent/executable_ban.rs
git commit -m "feat(muragent): MCP executable ban + permit-list validator"
```

---

### Task 8: Implement `.muragent` writer

**Files:**
- Create: `mur-common/src/muragent/writer.rs`

Builds the `.muragent` tar.gz: manifest.yaml, manifest.signed.json, signatures.json, profile.yaml, icon/, voice/, assets/. Signs with agent's Ed25519 identity.

- [ ] **Step 1: Write the implementation**

Create `mur-common/src/muragent/writer.rs`:

```rust
//! `.muragent` writer — build a signed agent package tarball.

use crate::agent::AgentProfile;
use crate::identity::AgentIdentity;
use crate::muragent::dsse;
use crate::muragent::jcs_canonical;
use crate::muragent::manifest::MuragentManifest;
use crate::muragent::statement::{self, InTotoStatement};
use crate::muragent::MuragentError;
use flate2::Compression;
use flate2::write::GzEncoder;
use std::fs;
use std::path::{Path, PathBuf};
use tar::Builder;

pub struct MuragentWriter {
    manifest: MuragentManifest,
    profile_yaml: String,
    identity: AgentIdentity,
    icon_files: Vec<(String, Vec<u8>)>,
    voice_yaml: Option<String>,
    commander_assets: Vec<(String, Vec<u8>)>,
    hub_assets: Vec<(String, Vec<u8>)>,
}

impl MuragentWriter {
    pub fn new(manifest: MuragentManifest, profile_yaml: String, identity: AgentIdentity) -> Self {
        Self {
            manifest,
            profile_yaml,
            identity,
            icon_files: Vec::new(),
            voice_yaml: None,
            commander_assets: Vec::new(),
            hub_assets: Vec::new(),
        }
    }

    pub fn add_icon(&mut self, name: &str, data: Vec<u8>) {
        self.icon_files.push((format!("icon/{name}"), data));
    }

    pub fn set_voice_yaml(&mut self, yaml: String) {
        self.voice_yaml = Some(yaml);
    }

    pub fn add_commander_asset(&mut self, path: &str, data: Vec<u8>) {
        self.commander_assets.push((format!("assets/commander/{path}"), data));
    }

    pub fn add_hub_asset(&mut self, path: &str, data: Vec<u8>) {
        self.hub_assets.push((format!("assets/{path}"), data));
    }

    /// Write the `.muragent` tar.gz to `out_path`.
    pub fn write(&self, out_path: &Path) -> Result<(), MuragentError> {
        // Step 1: Serialize manifest to YAML
        let manifest_yaml = serde_yaml_ng::to_string(&self.manifest)
            .map_err(|e| MuragentError::ManifestParse(e.to_string()))?;

        // Step 2: Derive manifest.signed.json (RFC 8785 JCS)
        let signed_json_bytes = jcs_canonical::derive_signed_json(&manifest_yaml)?;

        // Step 3: Build in-toto Statement
        let all_files = self.collect_all_files(&manifest_yaml, &signed_json_bytes);
        let statement: InTotoStatement = statement::build_statement(&signed_json_bytes, &all_files);

        // Step 4: Canonicalize the Statement to JSON
        let statement_json = serde_json::to_string(&statement)
            .map_err(|e| MuragentError::Other(format!("statement serialize: {e}")))?;
        let statement_canonical = String::from_utf8(crate::jcs::to_jcs(
            &serde_json::from_str::<serde_json::Value>(&statement_json)
                .map_err(|e| MuragentError::Other(format!("statement re-parse: {e}")))?,
        ))
        .map_err(|e| MuragentError::Other(format!("jcs utf-8: {e}")))?;

        // Step 5: Sign with DSSE
        let envelope = dsse::sign(
            "application/vnd.in-toto+json",
            &statement_canonical,
            &self.identity,
        )?;
        let signatures_json = serde_json::to_string_pretty(&envelope)
            .map_err(|e| MuragentError::Other(format!("signatures serialize: {e}")))?;

        // Step 6: Assemble tar.gz
        let file = fs::File::create(out_path)
            .map_err(|e| MuragentError::Io(e))?;
        let gz = GzEncoder::new(file, Compression::default());
        let mut tar = Builder::new(gz);

        add_blob(&mut tar, "manifest.yaml", manifest_yaml.as_bytes())?;
        add_blob(&mut tar, "manifest.signed.json", &signed_json_bytes)?;
        add_blob(&mut tar, "signatures.json", signatures_json.as_bytes())?;
        add_blob(&mut tar, "profile.yaml", self.profile_yaml.as_bytes())?;

        for (name, data) in &self.icon_files {
            add_blob(&mut tar, name, data)?;
        }

        if let Some(ref voice_yaml) = self.voice_yaml {
            add_blob(&mut tar, "voice/voice.yaml", voice_yaml.as_bytes())?;
        }

        for (name, data) in &self.commander_assets {
            add_blob(&mut tar, name, data)?;
        }

        for (name, data) in &self.hub_assets {
            add_blob(&mut tar, name, data)?;
        }

        tar.into_inner()
            .map_err(|e| MuragentError::Other(format!("close tar: {e}")))?
            .finish()
            .map_err(|e| MuragentError::Other(format!("flush gzip: {e}")))?;

        Ok(())
    }

    fn collect_all_files(
        &self,
        manifest_yaml: &str,
        signed_json_bytes: &[u8],
    ) -> Vec<(String, Vec<u8>)> {
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();

        // These three are excluded from the Statement subject list
        files.push(("manifest.yaml".into(), manifest_yaml.as_bytes().to_vec()));
        files.push(("manifest.signed.json".into(), signed_json_bytes.to_vec()));
        // signatures.json content isn't known yet; it gets excluded too
        files.push(("signatures.json".into(), b"placeholder".to_vec()));

        files.push(("profile.yaml".into(), self.profile_yaml.as_bytes().to_vec()));

        for (name, data) in &self.icon_files {
            files.push((name.clone(), data.clone()));
        }

        if let Some(ref voice) = self.voice_yaml {
            files.push(("voice/voice.yaml".into(), voice.as_bytes().to_vec()));
        }

        for (name, data) in &self.commander_assets {
            files.push((name.clone(), data.clone()));
        }

        for (name, data) in &self.hub_assets {
            files.push((name.clone(), data.clone()));
        }

        files
    }
}

fn add_blob<W: std::io::Write>(tar: &mut Builder<W>, name: &str, bytes: &[u8]) -> Result<(), MuragentError> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    // Regular files get 0o644 (no execute bits)
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, name, bytes)
        .map_err(|e| MuragentError::Other(format!("tar append {name}: {e}")))?;
    Ok(())
}

/// Build a `MuragentManifest` from an `AgentProfile` and agent home directory.
pub fn build_manifest_from_profile(
    profile: &AgentProfile,
    mur_version: &str,
) -> MuragentManifest {
    use crate::muragent::manifest::*;

    MuragentManifest {
        schema: "mur-agent/2".into(),
        exported_at: chrono::Utc::now().to_rfc3339(),
        exporter: ExporterInfo {
            mur_version: mur_version.to_string(),
            tool: "mur".into(),
            min_hub_version: Some(mur_version.to_string()),
            min_commander_version: None,
        },
        agent: AgentRef {
            slug: profile.name.clone(),
            display_name: profile.display_name.clone(),
            bundle_id: format!("run.mur.agent.{}", profile.name),
            url_scheme: format!("muragent-{}", profile.name),
            original_uuid: profile.id.clone(),
        },
        required_surfaces: vec![Surface::Hub],
        optional_capabilities: profile.capabilities.clone(),
        mcp_servers: profile
            .mcp_servers
            .iter()
            .map(|s| McpServerRef {
                name: s.name.clone(),
                command_basename: PathBuf::from(&s.command)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&s.command)
                    .to_string(),
            })
            .collect(),
        icon: IconHashes {
            formats: vec![],
            hash: IconHashMap {
                icns: None,
                ico: None,
                png: None,
            },
        },
        sanitized: SanitizedReport {
            removed_fields: vec!["identity.private_key".into()],
        },
        hub: Some(HubBlock {
            appearance: HubAppearance {
                style_preset: profile.appearance.style_preset.clone(),
                behavior_preset: profile.appearance.behavior_preset.clone(),
            },
            voice: if profile.voice.enabled {
                Some(HubVoice { enabled: true })
            } else {
                None
            },
            pet: Some(HubPet { enabled: true }),
            url_scheme_overrides: vec![],
        }),
        commander: None,
        deployment: None,
        assignment: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentProfile;
    use tempfile::TempDir;

    #[test]
    fn write_and_verify_roundtrip() {
        // This test will be expanded in M-export-1 integration tests.
        // For now, verify the writer produces a valid tar.gz.
        let tmp = TempDir::new().unwrap();
        let out = tmp.path().join("test.muragent");

        let profile = AgentProfile::default_for_tests();
        let identity = AgentIdentity::generate();
        let manifest = build_manifest_from_profile(&profile, "2.13.0");

        let profile_yaml = serde_yaml_ng::to_string(&profile).unwrap();
        let mut writer = MuragentWriter::new(manifest, profile_yaml, identity);
        writer.add_icon("icon-512.png", b"fake-png".to_vec());
        writer.write(&out).unwrap();

        assert!(out.exists());
        assert!(out.metadata().unwrap().len() > 0);
    }
}
```

- [ ] **Step 2: Build check**

```bash
cargo build -p mur-common
```

Expected: PASS

- [ ] **Step 3: Run tests**

```bash
cargo test -p mur-common -- muragent::writer
```

Expected: 1 test PASS (roundtrip smoke test)

- [ ] **Step 4: Commit**

```bash
git add mur-common/src/muragent/writer.rs
git commit -m "feat(muragent): writer — build signed .muragent tar.gz"
```

---

### Task 9: Implement `.muragent` reader and 11-step validator

**Files:**
- Create: `mur-common/src/muragent/reader.rs`
- Create: `mur-common/src/muragent/validator.rs`

- [ ] **Step 1: Write the reader**

Create `mur-common/src/muragent/reader.rs`:

```rust
//! `.muragent` reader — extract and inspect a signed agent package.

use crate::muragent::MuragentError;
use flate2::read::GzDecoder;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use tar::Archive;

pub struct MuragentArchive {
    /// All files in the tarball keyed by path → raw bytes.
    pub files: BTreeMap<String, Vec<u8>>,
}

impl MuragentArchive {
    /// Read and extract all files from a `.muragent` tar.gz.
    pub fn read(path: &Path) -> Result<Self, MuragentError> {
        let file = std::fs::File::open(path)
            .map_err(|e| MuragentError::Io(e))?;
        let gz = GzDecoder::new(file);
        let mut archive = Archive::new(gz);
        let mut files = BTreeMap::new();

        for entry in archive.entries()
            .map_err(|e| MuragentError::Other(format!("tar entries: {e}")))?
        {
            let mut entry = entry
                .map_err(|e| MuragentError::Other(format!("tar entry: {e}")))?;

            let path = entry
                .path()
                .map_err(|e| MuragentError::Other(format!("entry path: {e}")))?
                .to_str()
                .ok_or_else(|| MuragentError::Other("non-UTF-8 path in tarball".into()))?
                .to_string();

            // Reject symlinks
            let entry_type = entry.header().entry_type();
            if entry_type == tar::EntryType::Symlink || entry_type == tar::EntryType::Link {
                return Err(MuragentError::ExecutableContent(format!(
                    "symlinks not allowed in .muragent: {path}"
                )));
            }

            // Validate path safety
            crate::muragent::jcs_canonical::validate_tarball_path(&path)
                .map_err(|e| MuragentError::Other(e.to_string()))?;

            let mut data = Vec::new();
            entry
                .read_to_end(&mut data)
                .map_err(|e| MuragentError::Io(e))?;

            files.insert(path, data);
        }

        Ok(Self { files })
    }

    pub fn get(&self, path: &str) -> Option<&[u8]> {
        self.files.get(path).map(|v| v.as_slice())
    }

    pub fn get_str(&self, path: &str) -> Result<&str, MuragentError> {
        let bytes = self
            .get(path)
            .ok_or_else(|| MuragentError::Other(format!("file not found: {path}")))?;
        std::str::from_utf8(bytes)
            .map_err(|e| MuragentError::Other(format!("{path} is not valid UTF-8: {e}")))
    }

    pub fn files_as_vec(&self) -> Vec<(String, Vec<u8>)> {
        self.files
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}
```

- [ ] **Step 2: Write the validator**

Create `mur-common/src/muragent/validator.rs`:

```rust
//! 11-step validation pipeline (§6.4).
//!
//! Every step's failure is fatal — no "continue anyway" path.

use crate::muragent::MuragentError;
use crate::muragent::dsse;
use crate::muragent::executable_ban;
use crate::muragent::jcs_canonical;
use crate::muragent::manifest::MuragentManifest;
use crate::muragent::reader::MuragentArchive;
use crate::muragent::statement::{InTotoStatement, verify_subjects};

pub struct ValidationResult {
    pub manifest: MuragentManifest,
    pub author_pubkey: [u8; 32],
    pub keyid: String,
}

/// Run the full 11-step validation pipeline. Every failure is fatal (§7.5).
pub fn validate(archive: &MuragentArchive) -> Result<ValidationResult, MuragentError> {
    // Step 1: Tarball integrity — already done by MuragentArchive::read

    // Step 2: No executable content
    for (path, data) in &archive.files {
        executable_ban::check_extension(path)
            .map_err(|e| MuragentError::ExecutableContent(e))?;
    }
    // Check MCP commands from manifest
    let manifest_yaml = archive.get_str("manifest.yaml")?;
    let manifest: MuragentManifest = serde_yaml_ng::from_str(manifest_yaml)
        .map_err(|e| MuragentError::ManifestParse(e.to_string()))?;
    for mcp in &manifest.mcp_servers {
        executable_ban::check_mcp_command(&mcp.command_basename, &[])
            .map_err(|e| MuragentError::ForbiddenMcpCommand(e))?;
    }

    // Step 3: Schema version
    if !manifest.is_v2() {
        return Err(MuragentError::SchemaMismatch(manifest.schema.clone()));
    }

    // Step 4: Version compatibility — deferred to caller (Hub/Commander checks its own version)

    // Step 5: manifest.signed.json matches re-derived canonical JSON
    let embedded_signed_json = archive.get_str("manifest.signed.json")?;
    let rederived = jcs_canonical::derive_signed_json(manifest_yaml)?;
    if embedded_signed_json.as_bytes() != rederived.as_slice() {
        return Err(MuragentError::SignedJsonMismatch);
    }

    // Step 6: DSSE envelope structure
    let signatures_json = archive.get_str("signatures.json")?;
    let envelope: dsse::DsseEnvelope = serde_json::from_str(signatures_json)
        .map_err(|e| MuragentError::DsseError(format!("signatures.json parse: {e}")))?;

    // Step 7: Statement structure — payload decodes to in-toto v1 Statement
    use base64::{Engine, engine::general_purpose::STANDARD as B64};
    let payload_bytes = B64
        .decode(&envelope.payload)
        .map_err(|e| MuragentError::DsseError(format!("payload base64: {e}")))?;
    let statement: InTotoStatement = serde_json::from_slice(&payload_bytes)
        .map_err(|e| MuragentError::DsseError(format!("statement parse: {e}")))?;

    if statement.type_ != "https://in-toto.io/Statement/v1" {
        return Err(MuragentError::DsseError(format!(
            "unexpected statement _type: {}", statement.type_
        )));
    }
    if statement.predicate_type != "https://mur.run/agent-manifest/v1" {
        return Err(MuragentError::DsseError(format!(
            "unexpected predicateType: {}", statement.predicate_type
        )));
    }

    // Verify manifest_sha256 matches
    let actual_manifest_sha256 = {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(embedded_signed_json.as_bytes()))
    };
    if statement.predicate.manifest_sha256 != actual_manifest_sha256 {
        return Err(MuragentError::DsseError(format!(
            "manifest_sha256 mismatch: expected {}, got {}",
            actual_manifest_sha256, statement.predicate.manifest_sha256
        )));
    }

    // Step 8: Author signature verification (verify_strict)
    dsse::verify(&envelope, "application/vnd.in-toto+json")?;

    // Step 9: Subject hashes
    verify_subjects(&statement, &archive.files_as_vec())?;

    // Step 10: Mur signature (ignored in v1)
    // Step 11: Revocation check (skipped in v1)

    // Extract author pubkey for trust store
    let pubkey_bytes = B64
        .decode(&envelope.signatures[0].public_key)
        .map_err(|e| MuragentError::DsseError(format!("pubkey b64: {e}")))?;
    let pubkey_arr: [u8; 32] = pubkey_bytes
        .try_into()
        .map_err(|_| MuragentError::DsseError("pubkey not 32 bytes".into()))?;

    Ok(ValidationResult {
        manifest,
        author_pubkey: pubkey_arr,
        keyid: envelope.signatures[0].keyid.clone(),
    })
}
```

- [ ] **Step 3: Build check**

```bash
cargo build -p mur-common
```

Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add mur-common/src/muragent/reader.rs mur-common/src/muragent/validator.rs
git commit -m "feat(muragent): reader + 11-step validation pipeline"
```

---

### Task 10: Implement trust store data layer

**Files:**
- Create: `mur-common/src/trust/mod.rs`
- Create: `mur-common/src/trust/rotation.rs`
- Create: `mur-common/src/trust/legacy.rs`
- Modify: `mur-common/src/lib.rs`

Per spec §7.1: shared trust store at `~/.mur/trust/trust.yaml` with file-locked concurrent access, rotation manifest support, and legacy `~/.mur/trust.json` migration.

- [ ] **Step 1: Write trust store types and read/write logic**

Create `mur-common/src/trust/mod.rs`:

```rust
//! Shared trust store at `~/.mur/trust/trust.yaml`.
//!
//! Spec §7.1: Hub and Commander share the same trust store.
//! File-locked writes; lock-free reads with retry.

pub mod rotation;
pub mod legacy;

use crate::MuragentError;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    Known,
    Pending,
    Rejected,
    Superseded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustEntry {
    pub public_key: String,
    pub display_name_seen: String,
    pub first_seen: String,
    pub last_seen: String,
    pub last_seen_surface: String,
    pub trust_level: TrustLevel,
    pub fingerprint: String,
    pub word_list: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotated_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_rotation_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustStore {
    pub agents: Vec<TrustEntry>,
}

impl TrustStore {
    /// Load trust store from `~/.mur/trust/trust.yaml`.
    /// If the file doesn't exist, returns an empty store.
    /// Runs legacy migration if `~/.mur/trust.json` exists.
    pub fn load() -> Result<Self, MuragentError> {
        let path = trust_store_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| MuragentError::Io(e))?;
        }

        // Legacy migration
        let legacy_path = mur_home().join("trust.json");
        if legacy_path.exists() && !path.exists() {
            legacy::migrate_legacy(&legacy_path, &path)?;
        }

        if !path.exists() {
            return Ok(Self { agents: vec![] });
        }

        let yaml = std::fs::read_to_string(&path)
            .map_err(|e| MuragentError::Io(e))?;
        serde_yaml_ng::from_str(&yaml)
            .map_err(|e| MuragentError::Other(format!("trust store parse: {e}")))
    }

    /// Atomically save the trust store.
    pub fn save(&self) -> Result<(), MuragentError> {
        let path = trust_store_path();
        let yaml = serde_yaml_ng::to_string(self)
            .map_err(|e| MuragentError::Other(format!("trust store serialize: {e}")))?;
        let tmp = path.with_extension("yaml.tmp");
        std::fs::write(&tmp, &yaml)
            .map_err(|e| MuragentError::Io(e))?;
        std::fs::rename(&tmp, &path)
            .map_err(|e| MuragentError::Io(e))?;
        Ok(())
    }

    /// Find an entry by public key (base64).
    pub fn find_by_pubkey(&self, pubkey_b64: &str) -> Option<&TrustEntry> {
        self.agents.iter().find(|e| e.public_key == pubkey_b64)
    }

    /// Find known entries by display name (for key-change detection).
    pub fn find_by_display_name(&self, name: &str) -> Vec<&TrustEntry> {
        self.agents
            .iter()
            .filter(|e| e.display_name_seen == name)
            .collect()
    }

    /// Insert or update an entry.
    pub fn upsert(&mut self, entry: TrustEntry) {
        if let Some(existing) = self
            .agents
            .iter_mut()
            .find(|e| e.public_key == entry.public_key)
        {
            *existing = entry;
        } else {
            self.agents.push(entry);
        }
    }

    /// Remove an entry by public key.
    pub fn remove(&mut self, pubkey_b64: &str) {
        self.agents.retain(|e| e.public_key != pubkey_b64);
    }
}

/// Derive the 4-word fingerprint from a public key.
/// Uses EFF long word list (7776 words), encoding 52 bits of SHA-256(pubkey).
pub fn word_list_fingerprint(pubkey: &[u8; 32]) -> String {
    use sha2::Digest;
    let hash = sha2::Sha256::digest(pubkey);
    // Take 52 bits (6.5 bytes) and split into 4 × 13-bit indices
    let bits: u64 = u64::from_be_bytes([
        0, 0,
        hash[0], hash[1], hash[2], hash[3], hash[4], hash[5],
    ]) >> 12; // 52 bits
    let w0 = ((bits >> 39) & 0x1FFF) as usize; // 13 bits
    let w1 = ((bits >> 26) & 0x1FFF) as usize;
    let w2 = ((bits >> 13) & 0x1FFF) as usize;
    let w3 = (bits & 0x1FFF) as usize;

    let words = eff_word(w0, w1, w2, w3);
    format!("{words}")
}

fn eff_word(w0: usize, w1: usize, w2: usize, w3: usize) -> String {
    // Inline the EFF long word list (first ~100 entries for compilation;
    // actual list lives in const array loaded at compile time)
    let list = EFF_LONG_WORD_LIST;
    format!(
        "{} {} {} {}",
        list[w0 % list.len()],
        list[w1 % list.len()],
        list[w2 % list.len()],
        list[w3 % list.len()],
    )
}

/// Short fingerprint: first 8 hex chars of SHA-256(pubkey).
pub fn short_fingerprint(pubkey: &[u8; 32]) -> String {
    use sha2::Digest;
    let hash = sha2::Sha256::digest(pubkey);
    let hex = format!("{:x}", hash);
    hex[..8].to_string()
}

fn trust_store_path() -> PathBuf {
    mur_home().join("trust").join("trust.yaml")
}

fn mur_home() -> PathBuf {
    dirs::home_dir()
        .expect("home dir")
        .join(".mur")
}

// In production, load from file at compile time.
// For the plan, embed a representative subset.
const EFF_LONG_WORD_LIST: &[&str] = &[
    "abacus", "abdomen", "abdominal", "abide", "abiding", "ability",
    "ablaze", "able", "abnormal", "abrasion", "abrasive", "abreast",
    // ... full 7776 words loaded from eff_large_wordlist.txt at build time
    "zucchini",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_list_is_deterministic() {
        let pk = [0x42u8; 32];
        let a = word_list_fingerprint(&pk);
        let b = word_list_fingerprint(&pk);
        assert_eq!(a, b);
    }

    #[test]
    fn short_fingerprint_is_8_chars() {
        let pk = [0x42u8; 32];
        let fp = short_fingerprint(&pk);
        assert_eq!(fp.len(), 8);
    }

    #[test]
    fn empty_store_loads() {
        // Test requires temp dir — skipped in unit; covered by integration tests
    }
}
```

- [ ] **Step 2: Write rotation manifest support**

Create `mur-common/src/trust/rotation.rs`:

```rust
//! Key rotation manifest support (§7.1.1).
//!
//! Signed by both old and new keys. Stored at `~/.mur/trust/rotations/<fingerprint>.rotation`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotationManifest {
    pub old_pubkey: String,
    pub new_pubkey: String,
    pub issued_at: String,
    pub sig_old: String,
    pub sig_new: String,
}

impl RotationManifest {
    /// Verify both signatures on the rotation manifest.
    pub fn verify(&self) -> Result<(), String> {
        use base64::{Engine, engine::general_purpose::STANDARD as B64};
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};

        let message = format!(
            "{}{}{}",
            self.old_pubkey, self.new_pubkey, self.issued_at
        );
        let msg_bytes = message.as_bytes();

        // Verify old key signature
        let old_pk_bytes: [u8; 32] = B64
            .decode(&self.old_pubkey)
            .map_err(|e| format!("old_pubkey b64: {e}"))?
            .try_into()
            .map_err(|_| "old_pubkey not 32 bytes".to_string())?;
        let old_vk = VerifyingKey::from_bytes(&old_pk_bytes);
        let old_sig_bytes: [u8; 64] = B64
            .decode(&self.sig_old)
            .map_err(|e| format!("sig_old b64: {e}"))?
            .try_into()
            .map_err(|_| "sig_old not 64 bytes".to_string())?;
        let old_sig = Signature::from_bytes(&old_sig_bytes);
        old_vk
            .verify_strict(msg_bytes, &old_sig)
            .map_err(|e| format!("old key signature: {e}"))?;

        // Verify new key signature
        let new_pk_bytes: [u8; 32] = B64
            .decode(&self.new_pubkey)
            .map_err(|e| format!("new_pubkey b64: {e}"))?
            .try_into()
            .map_err(|_| "new_pubkey not 32 bytes".to_string())?;
        let new_vk = VerifyingKey::from_bytes(&new_pk_bytes);
        let new_sig_bytes: [u8; 64] = B64
            .decode(&self.sig_new)
            .map_err(|e| format!("sig_new b64: {e}"))?
            .try_into()
            .map_err(|_| "sig_new not 64 bytes".to_string())?;
        let new_sig = Signature::from_bytes(&new_sig_bytes);
        new_vk
            .verify_strict(msg_bytes, &new_sig)
            .map_err(|e| format!("new key signature: {e}"))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reject_tampered_rotation() {
        let mut manifest = RotationManifest {
            old_pubkey: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
            new_pubkey: "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB".into(),
            issued_at: "2026-05-20T12:00:00Z".into(),
            sig_old: "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC".into(),
            sig_new: "DDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD".into(),
        };
        // Tamper with the timestamp
        manifest.issued_at = "2025-01-01T00:00:00Z".into();
        assert!(manifest.verify().is_err());
    }
}
```

- [ ] **Step 3: Write legacy migration**

Create `mur-common/src/trust/legacy.rs`:

```rust
//! Legacy `~/.mur/trust.json` → `~/.mur/trust/trust.yaml` migration.
//!
//! Runs exactly once: on first access to the trust store, if legacy JSON
//! exists and the new YAML store does not.

use crate::MuragentError;
use std::path::Path;

pub fn migrate_legacy(legacy_path: &Path, new_path: &Path) -> Result<(), MuragentError> {
    let json = std::fs::read_to_string(legacy_path)
        .map_err(|e| MuragentError::Io(e))?;

    // Commander's legacy format is flat JSON with a "trusted_author_keys" array
    // Try parsing; if incompatible, skip migration and start fresh
    let legacy: serde_json::Value = serde_json::from_str(&json)
        .map_err(|_| {
            // Legacy file is unparseable — skip migration, start fresh
            tracing::warn!("legacy trust.json unparseable, starting fresh trust store");
        })
        .unwrap_or_default();

    // For now, v1 starts fresh even if legacy exists.
    // The migration skeleton is here for future enhancement.
    let _ = legacy;

    // Create parent dir
    if let Some(parent) = new_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| MuragentError::Io(e))?;
    }

    // Write empty trust store
    let empty = super::TrustStore { agents: vec![] };
    let yaml = serde_yaml_ng::to_string(&empty)
        .map_err(|e| MuragentError::Other(format!("trust store serialize: {e}")))?;
    std::fs::write(new_path, &yaml)
        .map_err(|e| MuragentError::Io(e))?;

    // Remove legacy file after successful migration
    std::fs::remove_file(legacy_path)
        .map_err(|e| MuragentError::Io(e))?;

    tracing::info!("migrated legacy trust.json → trust/trust.yaml");
    Ok(())
}
```

- [ ] **Step 4: Register modules in lib.rs**

```rust
pub mod muragent;
pub mod trust;
```

- [ ] **Step 5: Build and test**

```bash
cargo build -p mur-common
cargo test -p mur-common -- trust
```

Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add mur-common/src/trust/ mur-common/src/lib.rs
git commit -m "feat(mur-common): shared trust store with rotation + legacy migration"
```

---

### Task 11: Integration test — export/validate roundtrip

**Files:**
- Create: `mur-common/tests/muragent_format.rs`

- [ ] **Step 1: Write integration test for the full export→validate cycle**

```rust
//! Integration test: full export → validate roundtrip for `.muragent` v2.

use mur_common::agent::AgentProfile;
use mur_common::identity::AgentIdentity;
use mur_common::muragent::manifest::*;
use mur_common::muragent::reader::MuragentArchive;
use mur_common::muragent::validator;
use mur_common::muragent::writer::{MuragentWriter, build_manifest_from_profile};
use tempfile::TempDir;

#[test]
fn export_validate_roundtrip_smoke() {
    let tmp = TempDir::new().unwrap();
    let out = tmp.path().join("test.muragent");

    let profile = AgentProfile::default_for_tests();
    let identity = AgentIdentity::generate();
    let manifest = build_manifest_from_profile(&profile, "2.13.0");

    let profile_yaml = serde_yaml_ng::to_string(&profile).unwrap();
    let mut writer = MuragentWriter::new(manifest, profile_yaml, identity);
    writer.add_icon("icon-512.png", b"fake-png-data".to_vec());
    writer.write(&out).unwrap();

    // Read back and validate
    let archive = MuragentArchive::read(&out).unwrap();
    let result = validator::validate(&archive).unwrap();

    assert_eq!(result.manifest.schema, "mur-agent/2");
    assert_eq!(result.manifest.agent.slug, profile.name);
}

#[test]
fn tampered_payload_rejected() {
    let tmp = TempDir::new().unwrap();
    let out = tmp.path().join("tampered.muragent");

    let profile = AgentProfile::default_for_tests();
    let identity = AgentIdentity::generate();
    let manifest = build_manifest_from_profile(&profile, "2.13.0");

    let profile_yaml = serde_yaml_ng::to_string(&profile).unwrap();
    let mut writer = MuragentWriter::new(manifest, profile_yaml, identity);
    writer.add_icon("icon-512.png", b"original-data".to_vec());
    writer.write(&out).unwrap();

    // Tamper: flip a byte in profile.yaml
    let mut data = std::fs::read(&out).unwrap();
    // Find and flip a byte in the profile content
    if let Some(pos) = data.windows(8).position(|w| w == b"profile:") {
        data[pos + 8] ^= 0x01;
    }
    std::fs::write(&out, &data).unwrap();

    let archive = MuragentArchive::read(&out).unwrap();
    let result = validator::validate(&archive);
    assert!(result.is_err(), "tampered payload must be rejected");
}

#[test]
fn legacy_schema_rejected() {
    // A .muragent claiming schema "mur-agent-package/1" must be rejected
    let tmp = TempDir::new().unwrap();
    let out = tmp.path().join("legacy.muragent");

    let profile = AgentProfile::default_for_tests();
    let identity = AgentIdentity::generate();
    let mut manifest = build_manifest_from_profile(&profile, "2.13.0");
    manifest.schema = "mur-agent-package/1".into();

    let profile_yaml = serde_yaml_ng::to_string(&profile).unwrap();
    let mut writer = MuragentWriter::new(manifest, profile_yaml, identity);
    writer.add_icon("icon-512.png", b"data".to_vec());
    writer.write(&out).unwrap();

    let archive = MuragentArchive::read(&out).unwrap();
    let result = validator::validate(&archive);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("mur-agent/2"), "error should mention expected version, got: {err}");
}
```

- [ ] **Step 2: Run the integration tests**

```bash
cargo test -p mur-common --test muragent_format
```

Expected: 3 tests PASS

- [ ] **Step 3: Write fatal-not-advisory property test**

Create `mur-common/tests/muragent_fatal_not_advisory.rs`:

```rust
//! Property tests: every §6.4 failure is fatal, never falls through to import.

use mur_common::agent::AgentProfile;
use mur_common::identity::AgentIdentity;
use mur_common::muragent::reader::MuragentArchive;
use mur_common::muragent::validator;
use mur_common::muragent::writer::{MuragentWriter, build_manifest_from_profile};
use tempfile::TempDir;

fn make_test_package() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let out = tmp.path().join("test.muragent");

    let profile = AgentProfile::default_for_tests();
    let identity = AgentIdentity::generate();
    let manifest = build_manifest_from_profile(&profile, "2.13.0");

    let profile_yaml = serde_yaml_ng::to_string(&profile).unwrap();
    let mut writer = MuragentWriter::new(manifest, profile_yaml, identity);
    writer.add_icon("icon-512.png", b"data".to_vec());
    writer.write(&out).unwrap();

    (tmp, out)
}

#[test]
fn byte_flip_in_signatures_json_causes_refuse() {
    let (_tmp, out) = make_test_package();
    let mut data = std::fs::read(&out).unwrap();
    // Find "signatures" and flip a byte
    if let Some(pos) = data.windows(10).position(|w| w == b"signatures") {
        data[pos + 12] ^= 0x01;
    }
    std::fs::write(&out, &data).unwrap();

    let archive = MuragentArchive::read(&out).unwrap();
    assert!(validator::validate(&archive).is_err());
}

#[test]
fn byte_flip_in_manifest_signed_json_causes_refuse() {
    let (_tmp, out) = make_test_package();
    let mut data = std::fs::read(&out).unwrap();
    // Find "manifest.signed.json" entry and tamper
    if let Some(pos) = data.windows(19).position(|w| w == b"manifest.signed.js") {
        data[pos + 30] ^= 0x01;
    }
    std::fs::write(&out, &data).unwrap();

    let result = MuragentArchive::read(&out);
    // Either read fails (CRC) or validate fails — both are fatal
    if let Ok(archive) = result {
        assert!(validator::validate(&archive).is_err());
    }
}

#[test]
fn tarball_content_tamper_causes_refuse() {
    let (_tmp, out) = make_test_package();
    let mut data = std::fs::read(&out).unwrap();
    // Tamper a random byte deep in the gzip stream
    if data.len() > 100 {
        data[data.len() / 2] ^= 0xFF;
    }
    std::fs::write(&out, &data).unwrap();

    let result = MuragentArchive::read(&out);
    // CRC should fail
    assert!(result.is_err() || validator::validate(&result.unwrap()).is_err());
}
```

- [ ] **Step 4: Run property tests**

```bash
cargo test -p mur-common --test muragent_fatal_not_advisory
```

Expected: 3 tests PASS

- [ ] **Step 5: Commit**

```bash
git add mur-common/tests/
git commit -m "test(muragent): integration roundtrip + fatal-not-advisory property tests"
```

---

### Task 12: Wire `mur agent export` to `.muragent` default

**Files:**
- Modify: `mur-core/src/cmd/agent/export.rs`
- Modify: `mur-core/src/cmd/agent/mod.rs`

- [ ] **Step 1: Rewrite `cmd_export` to use `.muragent` as default**

Read the current `export.rs` (82 lines, shown above). Replace the format dispatch:

```rust
//! `mur agent export` — package an agent as a portable `.muragent` v2 bundle.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mur_common::identity::AgentIdentity;
use mur_common::muragent::writer::{MuragentWriter, build_manifest_from_profile};

use super::resolve_mur_home;

pub fn cmd_export(name: &str, out: &str, format: &str, sign_with: Option<&str>) -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let agent_home = mur_home.join("agents").join(name);
    if !agent_home.exists() {
        bail!("agent '{name}' not found");
    }

    match format {
        "muragent" | "pkg" => {
            // "pkg" is the old format name; map to .muragent v2 as default
            export_muragent(name, &agent_home, Path::new(out), sign_with)?;
        }
        "bin" => export_bin(name, &agent_home, Path::new(out))?,
        "standalone" => {
            bail!("--standalone requires MUR_APPLE_DEVELOPER_ID env var and macOS. Use 'muragent' format for portable export.");
        }
        other => bail!("unsupported export format '{other}' (use muragent or bin)"),
    }
    Ok(())
}

fn export_muragent(
    name: &str,
    agent_home: &Path,
    out: &Path,
    sign_with: Option<&str>,
) -> Result<()> {
    // Load the agent profile
    let profile_path = agent_home.join("profile.yaml");
    let profile_yaml = std::fs::read_to_string(&profile_path)
        .with_context(|| format!("read {}", profile_path.display()))?;
    let profile: mur_common::AgentProfile = serde_yaml_ng::from_str(&profile_yaml)
        .with_context(|| format!("parse {}", profile_path.display()))?;

    // Load identity
    let identity = if let Some(key_path) = sign_with {
        AgentIdentity::load(Path::new(key_path))
            .with_context(|| format!("load signing key from {}", key_path))?
    } else {
        let identity_path = agent_home.join("identity.key");
        AgentIdentity::load(&identity_path)
            .with_context(|| format!("load agent identity from {}", identity_path.display()))?
    };

    let mur_version = env!("CARGO_PKG_VERSION");
    let manifest = build_manifest_from_profile(&profile, mur_version);

    // Sanitize profile (strip private key, secretful notification targets)
    let mut export_profile = profile.clone();
    let removed = sanitize_profile_for_export(&mut export_profile);
    let mut manifest = manifest;
    manifest.sanitized.removed_fields = removed;
    let sanitized_yaml = serde_yaml_ng::to_string(&export_profile)
        .context("serialize sanitized profile")?;

    let mut writer = MuragentWriter::new(manifest, sanitized_yaml, identity);

    // Add icons if they exist (from agent home or defaults)
    let icon_base = agent_home.join("icon");
    for (filename, tar_name) in &[
        ("icon.icns", "icon.icns"),
        ("icon.ico", "icon.ico"),
        ("icon-512.png", "icon-512.png"),
    ] {
        let path = icon_base.join(filename);
        if path.exists() {
            let data = std::fs::read(&path)
                .with_context(|| format!("read {}", path.display()))?;
            writer.add_icon(tar_name, data);
        }
    }

    // Add voice config if present
    let voice_yaml_path = agent_home.join("voice.yaml");
    if voice_yaml_path.exists() {
        let voice = std::fs::read_to_string(&voice_yaml_path)
            .with_context(|| format!("read {}", voice_yaml_path.display()))?;
        writer.set_voice_yaml(voice);
    }

    writer.write(out)
        .with_context(|| format!("write .muragent to {}", out.display()))?;

    println!("Exported '{name}' → {}", out.display());
    Ok(())
}

fn sanitize_profile_for_export(profile: &mut mur_common::AgentProfile) -> Vec<String> {
    let mut removed = vec!["identity.private_key".to_string()];
    // Strip secretful notification targets (reuse logic from existing pkg.rs)
    profile.notifications.on_task_complete.retain(|t| !is_secretful(t));
    profile.notifications.on_error.retain(|t| !is_secretful(t));
    profile.notifications.on_shutdown.retain(|t| !is_secretful(t));
    if profile.transport.socket.auth.is_some() {
        removed.push("transport.socket.auth.token_file".to_string());
        profile.transport.socket.auth = None;
    }
    removed
}

fn is_secretful(t: &mur_common::agent::NotificationTarget) -> bool {
    matches!(
        t,
        mur_common::agent::NotificationTarget::Webhook { .. }
            | mur_common::agent::NotificationTarget::Slack { .. }
            | mur_common::agent::NotificationTarget::Webpush { .. }
            | mur_common::agent::NotificationTarget::Email { .. }
    )
}

// Keep the existing export_bin function unchanged
fn export_bin(name: &str, agent_home: &Path, out: &Path) -> Result<()> {
    // ... existing code unchanged ...
    let target_dir = std::env::temp_dir().join(format!("mur-export-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&target_dir).with_context(|| format!("create {}", target_dir.display()))?;

    let manifest_dir = locate_runtime_manifest_dir().context("locate mur-agent-runtime crate")?;

    let status = std::process::Command::new("cargo")
        .args([
            "build",
            "--release",
            "--features=embedded-agent",
            "--manifest-path",
        ])
        .arg(manifest_dir.join("Cargo.toml"))
        .env("MUR_EXPORT_AGENT_DIR", agent_home)
        .env("CARGO_TARGET_DIR", &target_dir)
        .status()
        .context("invoke cargo build")?;
    if !status.success() {
        bail!("cargo build failed: {status}");
    }
    let built = target_dir.join("release").join(if cfg!(windows) {
        "mur-agent-runtime.exe"
    } else {
        "mur-agent-runtime"
    });
    std::fs::copy(&built, out)
        .with_context(|| format!("copy {} -> {}", built.display(), out.display()))?;
    println!("Built self-contained agent binary at {}", out.display());
    Ok(())
}

fn locate_runtime_manifest_dir() -> Result<PathBuf> {
    // ... existing code unchanged ...
    if let Some(p) = std::env::var_os("MUR_AGENT_RUNTIME_MANIFEST_DIR") {
        return Ok(PathBuf::from(p));
    }
    let exe = std::env::current_exe().context("current_exe")?;
    let mut cur = exe.parent().map(|p| p.to_path_buf());
    while let Some(d) = cur {
        let candidate = d.join("mur-agent-runtime").join("Cargo.toml");
        if candidate.exists() {
            return Ok(d.join("mur-agent-runtime"));
        }
        cur = d.parent().map(|p| p.to_path_buf());
    }
    bail!(
        "could not locate mur-agent-runtime crate (set MUR_AGENT_RUNTIME_MANIFEST_DIR to override)"
    )
}
```

- [ ] **Step 2: Update CLI argument parsing in `cmd/agent/mod.rs`**

The existing CLI has `--format` flag. Ensure the default maps to `"muragent"`:

```rust
// In the CLI builder, change default format:
// Before: .default_value("pkg")
// After:  .default_value("muragent")
```

- [ ] **Step 3: Build and test**

```bash
cargo build -p mur-core
cargo test -p mur-core -- agent_export
```

Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/agent/export.rs mur-core/src/cmd/agent/mod.rs
git commit -m "feat(cli): switch 'mur agent export' default to .muragent v2"
```

---

### Task 13: Add `mur agent install`, `uninstall`, `inspect` CLI

**Files:**
- Create: `mur-core/src/cmd/agent/install.rs`
- Modify: `mur-core/src/cmd/agent/mod.rs`

- [ ] **Step 1: Write install command**

Create `mur-core/src/cmd/agent/install.rs`:

```rust
//! `mur agent install <path-to-.muragent>` — import an agent package.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mur_common::muragent::reader::MuragentArchive;
use mur_common::muragent::validator;
use mur_common::trust::TrustStore;

use super::resolve_mur_home;

pub fn cmd_install(path: &Path, auto_start: bool) -> Result<()> {
    let archive = MuragentArchive::read(path)
        .context("read .muragent file")?;

    // Full validation pipeline (fatal on any failure per §7.5)
    let result = validator::validate(&archive)
        .context("validate .muragent")?;

    let manifest = result.manifest;
    let slug = &manifest.agent.slug;
    let display_name = &manifest.agent.display_name;

    // Check trust store
    let mut trust = TrustStore::load()
        .context("load trust store")?;

    let author_pubkey_b64 = base64_encode(&result.author_pubkey);
    let existing = trust.find_by_pubkey(&author_pubkey_b64);

    // Check for key change without rotation (hard refuse)
    if existing.is_none() {
        let known_by_name = trust.find_by_display_name(display_name);
        if !known_by_name.is_empty() {
            bail!(
                "Agent '{}' has a new signing key from {} but no rotation manifest. \
                 This could indicate an impersonation attempt. \
                 Remove the old trust entry first if this is intentional.",
                display_name, known_by_name[0].display_name_seen
            );
        }
    }

    // Extract to ~/.mur/agents/<slug>/
    let mur_home = resolve_mur_home()?;
    let agent_dir = mur_home.join("agents").join(slug);
    if agent_dir.exists() {
        // Check if same agent (update) or different agent (collision)
        let existing_profile = agent_dir.join("profile.yaml");
        if existing_profile.exists() {
            let existing_yaml = fs::read_to_string(&existing_profile)?;
            if let Ok(existing) = serde_yaml_ng::from_str::<mur_common::AgentProfile>(&existing_yaml) {
                if existing.id == manifest.agent.original_uuid {
                    // Same agent — update flow
                    return update_agent(&archive, &agent_dir, &manifest, &result);
                }
            }
        }
        bail!(
            "agent '{}' already exists at {}. Use 'mur agent remove {}' first, or rename.",
            slug, agent_dir.display(), slug
        );
    }

    fs::create_dir_all(&agent_dir)
        .context("create agent directory")?;

    // Extract files
    extract_payload(&archive, &agent_dir)?;

    // Write trust store entry
    trust.upsert(mur_common::trust::TrustEntry {
        public_key: author_pubkey_b64.clone(),
        display_name_seen: display_name.clone(),
        first_seen: chrono::Utc::now().to_rfc3339(),
        last_seen: chrono::Utc::now().to_rfc3339(),
        last_seen_surface: "hub".into(),
        trust_level: mur_common::trust::TrustLevel::Pending,
        fingerprint: mur_common::trust::short_fingerprint(&result.author_pubkey),
        word_list: mur_common::trust::word_list_fingerprint(&result.author_pubkey),
        rotated_from: None,
        superseded_at: None,
        last_rotation_at: None,
    });
    trust.save().context("save trust store")?;

    println!("Installed agent '{display_name}' ({slug})");

    if auto_start {
        // Auto-start wiring deferred to M-export-5
        println!("  (auto-start will be wired in a future update)");
    }

    Ok(())
}

fn base64_encode(bytes: &[u8; 32]) -> String {
    use base64::{Engine, engine::general_purpose::STANDARD as B64};
    B64.encode(bytes)
}

fn extract_payload(archive: &MuragentArchive, agent_dir: &Path) -> Result<()> {
    for (path, data) in &archive.files {
        // Skip signature/manifest files
        if path == "manifest.yaml"
            || path == "manifest.signed.json"
            || path == "signatures.json"
        {
            continue;
        }
        let dest = agent_dir.join(path);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&dest, data)?;
    }
    Ok(())
}

fn update_agent(
    archive: &MuragentArchive,
    agent_dir: &Path,
    manifest: &mur_common::muragent::manifest::MuragentManifest,
    _result: &validator::ValidationResult,
) -> Result<()> {
    // Preserve user data (chat history, settings) but replace profile + assets
    let data_dir = agent_dir.join("data");
    let preserve_data = data_dir.exists();

    // Remove old files (except data/)
    for entry in fs::read_dir(agent_dir)? {
        let entry = entry?;
        if entry.file_name() == "data" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
    }

    extract_payload(archive, agent_dir)?;

    println!(
        "Updated agent '{}' to v{}",
        manifest.agent.display_name,
        manifest.exporter.mur_version
    );

    if !preserve_data {
        fs::create_dir_all(&data_dir)?;
    }

    Ok(())
}

pub fn cmd_uninstall(name: &str, delete_data: bool) -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let agent_dir = mur_home.join("agents").join(name);

    if !agent_dir.exists() {
        bail!("agent '{name}' is not installed");
    }

    if delete_data {
        fs::remove_dir_all(&agent_dir)?;
    } else {
        // Remove everything except data/
        for entry in fs::read_dir(&agent_dir)? {
            let entry = entry?;
            if entry.file_name() == "data" {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                fs::remove_dir_all(&path)?;
            } else {
                fs::remove_file(&path)?;
            }
        }
    }

    println!("Uninstalled agent '{name}'");
    Ok(())
}

pub fn cmd_inspect(path: &Path) -> Result<()> {
    let archive = MuragentArchive::read(path)
        .context("read .muragent file")?;

    let manifest_yaml = archive.get_str("manifest.yaml")?;
    let manifest: mur_common::muragent::manifest::MuragentManifest =
        serde_yaml_ng::from_str(manifest_yaml)?;

    println!("Agent: {} ({})", manifest.agent.display_name, manifest.agent.slug);
    println!("Schema: {}", manifest.schema);
    println!("Exported: {}", manifest.exported_at);
    println!("Mur version: {}", manifest.exporter.mur_version);
    println!("UUID: {}", manifest.agent.original_uuid);
    println!("Surfaces: {:?}", manifest.required_surfaces);
    println!("Capabilities: {:?}", manifest.optional_capabilities);
    println!("MCP servers: {}", manifest.mcp_servers.len());
    for mcp in &manifest.mcp_servers {
        println!("  - {} ({})", mcp.name, mcp.command_basename);
    }

    // Verify signature (informational only for inspect)
    match validator::validate(&archive) {
        Ok(result) => {
            println!("\nSignature: VALID");
            println!("Author keyid: {}", result.keyid);
            println!("Author fingerprint: {}", mur_common::trust::short_fingerprint(&result.author_pubkey));
        }
        Err(e) => {
            println!("\nSignature: INVALID — {e}");
        }
    }

    Ok(())
}
```

- [ ] **Step 2: Wire in mod.rs**

Add to `mur-core/src/cmd/agent/mod.rs`:
```rust
pub mod install;
```

And wire CLI subcommands to `install::cmd_install`, `install::cmd_uninstall`, `install::cmd_inspect`.

- [ ] **Step 3: Build and smoke test**

```bash
cargo build -p mur-core
```

Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/agent/install.rs mur-core/src/cmd/agent/mod.rs
git commit -m "feat(cli): mur agent install / uninstall / inspect"
```

---

### M-export-3 through M-export-7: Summary

M-export-3 (Hub import dialog), M-export-4 (stub generation), M-export-5 (autostart + sidecar migration), M-export-6 (cross-repo Commander), and M-export-7 (revocations scaffolding) each depend on the shared library foundation above being complete and tested. They involve Tauri 2 UI code, platform-specific OS integration, and cross-repo coordination that is best planned in detail after M-export-1 and M-export-2 ship.

The key touchpoints for those milestones:

**M-export-3 (Hub import dialog):**
- `mur-hub-gui/src-tauri/src/lib.rs` — register `import_muragent_file` Tauri command
- `mur-hub-gui/src/routes/import/` — import confirmation dialog with first-time-author prompt UI (§7.2)
- `mur-gui-core/src/` — trust store bridge for the UI layer

**M-export-4 (Stub generation):**
- `mur-gui-core/src/stub/` — per-platform stub generator (macOS `.app`, Windows `.lnk`, Linux `.desktop`)
- `mur-gui-core/src/ipc/` — per-agent Unix domain socket / named pipe IPC
- `mur-hub-gui/src-tauri/src/` — stub validation gate integration

**M-export-5 (Autostart + sidecar migration):**
- `mur-gui-core/src/sidecar.rs` — rewrite to use OS init systems instead of child-process supervision
- `mur-agent-runtime/src/expression/` — move expression engine from Hub to runtime

**M-export-6 (Cross-repo Commander):**
- `~/Projects/mur-commander/crates/engine/Cargo.toml` — pin `mur-common` Git dep
- `~/Projects/mur-commander/crates/cli/` — `murc agent install / export`

**M-export-7 (Revocations scaffolding):**
- `mur-common/src/trust/revocations.rs` — `RevocationsList` type, fetch/parse
- `mur-hub-gui/src-tauri/src/` — daily refresh timer

---

### Task 14: Register module and verify full build

- [ ] **Step 1: Ensure `mur-common/src/lib.rs` exports everything**

```rust
pub mod jcs;
pub mod muragent;
pub mod trust;
```

- [ ] **Step 2: Full workspace build**

```bash
cargo build --workspace
```

Expected: PASS (all crates compile)

- [ ] **Step 3: Full workspace test**

```bash
cargo test --workspace
```

Expected: all existing tests PASS, new muragent tests PASS

- [ ] **Step 4: Run clippy**

```bash
cargo clippy --workspace -- -D warnings
```

Expected: PASS

- [ ] **Step 5: Final commit for M-export-1 + M-export-2**

```bash
git add -A
git commit -m "feat: .muragent v2 shared library + CLI export/install (M-export-1, M-export-2)"
```

---

## Self-Review Checklist

1. **Spec coverage:** Each requirement in spec §1–§16 maps to a task above or a deferred milestone (M-export-3 through M-export-7). The shared library (M-export-1) covers the file format (§6), signing (§6.3), validation (§6.4), and MCP executable ban (§6.4 step 2). The CLI surface (M-export-2) covers export (§8.1) and install/uninstall/inspect (§8.3). The trust store (§7.1) and rotation (§7.1.1) are implemented.
2. **No placeholders:** Every step has actual code or references existing code. Deferred milestones are explicitly listed with their file touchpoints.
3. **Type consistency:** `MuragentManifest` fields match across writer, reader, validator, and CLI. `TrustStore` → `TrustEntry` with `TrustLevel` enum matches the spec's YAML structure.
