//! Install a registry skill onto a specific agent — per-agent sibling of
//! `mcp_registry::cmd_mcp_registry_add`. Reuses the existing git registry +
//! resolver + per-agent installer, adding verify-on-install + Sandboxed trust.
//!
//! Public entry points are consumed by the CLI (Task 3) and Hub GUI; they are
//! wired there, so `dead_code` is expected here.

use std::path::Path;

use anyhow::{Result, bail};
use semver::Version;

use super::skill_verify::{HashStatus, SignatureStatus, VerifyOutcome, verify_skill_install};
use crate::cmd::skill_registry;
use mur_common::skill::local::set_trust_level;
use mur_common::skill::{TrustLevel, parse_canonical, scan::scan_skill};

// ─── Consent / view types ──────────────────────────────────────────────────

/// Full consent bundle shown to the user before install.
/// Serialisable so the Hub can display it in its modal.
#[allow(dead_code)] // wired by the CLI/Hub units (Task 3 / Hub PR)
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConsentInfo {
    pub name: String,
    pub version: String,
    pub publisher: String,
    /// String form of `RegistrySkillEntry.category` (already a plain string in the index).
    pub category: String,
    pub signature: SigView,
    /// `"match"` | `"mismatch"` | `"absent"`
    pub hash: String,
    pub mcp_requirements: Vec<String>,
    /// Human-readable findings from `ContentScanReport::human_summary()`.
    pub findings: Vec<String>,
    /// Hard failure — abort unless explicitly overridden.
    pub blocking: bool,
    /// Not proven-bad but not proven-good — requires `--yes` acknowledgement.
    pub needs_ack: bool,
    /// Raw YAML body of the resolved skill file (written to tmp → `cmd_skill_add`).
    pub body: String,
}

/// Serialisable view of `SignatureStatus`.
#[allow(dead_code)] // wired by the CLI/Hub units
#[derive(Debug, Clone, serde::Serialize)]
pub struct SigView {
    /// `"verified"` | `"unsigned"` | `"invalid"`
    pub status: String,
    pub publisher: String,
    pub key_fp: String,
}

/// Serialisable registry entry view for the Hub search panel.
#[allow(dead_code)] // wired by the CLI/Hub units
#[derive(Debug, Clone, serde::Serialize)]
pub struct RegistrySkillEntryView {
    pub name: String,
    pub description: String,
    pub publisher: String,
    pub category: String,
    pub latest: String,
    /// `true` when `content_sha256` in the index is non-empty.
    pub signed_in_index: bool,
}

// ─── Internal converters ──────────────────────────────────────────────────

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
        SignatureStatus::Unsigned => SigView {
            status: "unsigned".into(),
            publisher: String::new(),
            key_fp: String::new(),
        },
        SignatureStatus::Invalid => SigView {
            status: "invalid".into(),
            publisher: String::new(),
            key_fp: String::new(),
        },
    }
}

// ─── TEST SEAM — takes registry dir directly, no git/network ─────────────

/// Resolve a skill from a local registry directory into a `ConsentInfo`
/// (hash-check + signature-verify + content-scan). Does **not** install.
///
/// This is the network-free test seam. `resolve_consent` wraps it after
/// calling `fetch_and_load` to obtain the registry dir.
pub fn resolve_consent_in(
    registry_dir: &Path,
    skill: &str,
    version: Option<&str>,
) -> Result<ConsentInfo> {
    let idx = skill_registry::load_index(registry_dir)?;
    let entry = idx
        .skills
        .get(skill)
        .ok_or_else(|| anyhow::anyhow!("skill '{skill}' not found in registry"))?;

    let ver = match version {
        Some(v) => v.to_string(),
        None => entry.latest.clone(),
    };

    // Version existence check. When `ver == entry.latest` we allow the
    // versions-dir to be absent (caller may omit it in minimal registries).
    let avail = skill_registry::available_versions(registry_dir, skill)?;
    if !avail.iter().any(|v| v.to_string() == ver) && ver != entry.latest {
        bail!("version '{ver}' of '{skill}' not in registry (available: {avail:?})");
    }

    let _ = Version::parse(&ver); // tolerate non-semver — just a sanity log hook

    let path = skill_registry::skill_yaml_path(registry_dir, skill, &ver);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;

    let manifest =
        parse_canonical(&text).map_err(|e| anyhow::anyhow!("parse skill manifest: {e}"))?;

    let outcome: VerifyOutcome = verify_skill_install(&manifest, &text, &entry.content_sha256);
    let report = scan_skill(&manifest).map_err(|e| anyhow::anyhow!("content scan: {e}"))?;

    Ok(ConsentInfo {
        name: manifest.name.clone(),
        version: ver,
        publisher: entry.publisher.clone(),
        category: entry.category.clone(),
        signature: sig_view(&outcome.signature),
        hash: hash_str(&outcome.hash).into(),
        mcp_requirements: manifest
            .mcp_requirements
            .iter()
            .map(|r| format!("{} ({})", r.tool_pattern, r.capability.as_str()))
            .collect(),
        // ContentScanReport has no flat `findings` field — use human_summary().
        findings: report.human_summary(),
        blocking: outcome.is_blocking(),
        needs_ack: outcome.needs_ack(),
        body: text,
    })
}

// ─── Public entry points ───────────────────────────────────────────────────

/// Fetch the registry (git) then resolve consent. Hub preview + CLI confirm
/// both call this; neither installs until the gate passes.
#[allow(dead_code)] // wired by the CLI/Hub units
pub fn resolve_consent(mur_home: &Path, skill: &str, version: Option<&str>) -> Result<ConsentInfo> {
    let (dir, _idx) = skill_registry::fetch_and_load(mur_home, skill_registry::DEFAULT_REGISTRY)?;
    resolve_consent_in(&dir, skill, version)
}

/// Install a registry skill onto a specific agent at `TrustLevel::Sandboxed`.
///
/// Fail-closed gate:
/// - `blocking`  → abort unconditionally.
/// - `needs_ack && !accept` → abort with a `--yes` hint.
///
/// On success returns the skill path relative to the agent home (`"skills/<name>"`).
#[allow(dead_code)] // wired by the CLI/Hub units
pub async fn cmd_skill_registry_add(
    agent: &str,
    skill: &str,
    version: Option<&str>,
    accept: bool,
) -> Result<String> {
    let mur_home = super::resolve_mur_home()?;
    let consent = resolve_consent(&mur_home, skill, version)?;

    // Fail-closed gate.
    if consent.blocking {
        bail!(
            "skill '{}' has blocking security findings — install refused.\nFindings:\n  {}",
            consent.name,
            consent.findings.join("\n  ")
        );
    }
    if consent.needs_ack && !accept {
        bail!(
            "skill '{}' is unsigned or unhashed — pass --yes to acknowledge and install",
            consent.name
        );
    }

    // Write to a temp file so `cmd_skill_add` can read it (it expects a path).
    let tmp_dir = tempfile::tempdir()?;
    let tmp_path = tmp_dir.path().join(format!("{}.yaml", consent.name));
    std::fs::write(&tmp_path, &consent.body)?;
    super::skill::cmd_skill_add(agent, &tmp_path.to_string_lossy())?;

    // Enforce Sandboxed trust regardless of cmd_skill_add's default.
    set_trust_level(&mur_home, &consent.name, TrustLevel::Sandboxed)
        .map_err(|e| anyhow::anyhow!("set trust level: {e}"))?;

    Ok(format!("skills/{}", consent.name))
}

/// Search the registry and return serialisable views (Hub search panel + CLI).
#[allow(dead_code)] // wired by the CLI/Hub units
pub fn registry_search_for_agent(
    mur_home: &Path,
    query: &str,
) -> Result<Vec<RegistrySkillEntryView>> {
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

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // ── fixture helpers ─────────────────────────────────────────────────

    const SKILL_YAML: &str = r#"name: test-skill
version: 1.0.0
publisher: human:tester
description: A test skill
category: context
content:
  abstract: Does something useful
  context: Use this when you need to do something.
"#;

    fn sha256_hex(s: &str) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(s.as_bytes()))
    }

    /// Build a minimal registry layout under `dir`.
    /// `content_sha256` — pass `""` for absent, or `sha256_hex(SKILL_YAML)` for match,
    ///                     or any other string for mismatch.
    fn fixture_registry(dir: &std::path::Path, content_sha256: &str) {
        let index_yaml = format!(
            "skills:\n  test-skill:\n    latest: 1.0.0\n    description: A test skill\n    publisher: human:tester\n    category: context\n    tags: []\n    content_sha256: \"{content_sha256}\"\n    install_count: 0\n"
        );
        fs::write(dir.join("index.yaml"), &index_yaml).unwrap();

        let versions_dir = dir.join("skills").join("test-skill").join("versions");
        fs::create_dir_all(&versions_dir).unwrap();
        fs::write(versions_dir.join("1.0.0.yaml"), SKILL_YAML).unwrap();
    }

    // ── resolve_consent_in tests ────────────────────────────────────────

    #[test]
    fn absent_sha256_gives_needs_ack_not_blocking() {
        let tmp = tempfile::tempdir().unwrap();
        fixture_registry(tmp.path(), "");

        let consent = resolve_consent_in(tmp.path(), "test-skill", None).unwrap();

        assert_eq!(consent.name, "test-skill");
        assert_eq!(consent.version, "1.0.0");
        assert_eq!(consent.publisher, "human:tester");
        assert_eq!(consent.category, "context");
        assert_eq!(consent.hash, "absent");
        assert_eq!(consent.signature.status, "unsigned");
        assert!(!consent.blocking, "absent hash is not blocking");
        assert!(consent.needs_ack, "absent hash requires --yes ack");
        assert!(!consent.body.is_empty());
    }

    #[test]
    fn matching_sha256_not_blocking_and_no_ack_needed_when_also_unsigned() {
        // Hash match + unsigned → needs_ack (unsigned still requires --yes).
        let tmp = tempfile::tempdir().unwrap();
        let sha = sha256_hex(SKILL_YAML);
        fixture_registry(tmp.path(), &sha);

        let consent = resolve_consent_in(tmp.path(), "test-skill", None).unwrap();

        assert_eq!(consent.hash, "match");
        assert_eq!(consent.signature.status, "unsigned");
        assert!(!consent.blocking);
        // Hash matches but signature is unsigned → needs_ack still true.
        assert!(consent.needs_ack);
    }

    #[test]
    fn mismatched_sha256_is_blocking() {
        let tmp = tempfile::tempdir().unwrap();
        fixture_registry(tmp.path(), "deadbeef0000000000000000bad");

        let consent = resolve_consent_in(tmp.path(), "test-skill", None).unwrap();

        assert_eq!(consent.hash, "mismatch");
        assert!(consent.blocking, "mismatch must be blocking");
        assert!(!consent.needs_ack, "blocking overrides needs_ack");
    }

    #[test]
    fn skill_not_in_index_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("index.yaml"), "skills: {}\n").unwrap();

        let err = resolve_consent_in(tmp.path(), "nonexistent", None).unwrap_err();
        assert!(err.to_string().contains("not found in registry"));
    }

    #[test]
    fn explicit_version_not_available_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        // Index says latest=1.0.0; file only has 1.0.0.yaml.
        fixture_registry(tmp.path(), "");

        // Request a version that doesn't exist and is not `latest`.
        let err = resolve_consent_in(tmp.path(), "test-skill", Some("9.9.9")).unwrap_err();
        assert!(
            err.to_string().contains("not in registry"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn explicit_version_resolves_correctly() {
        let tmp = tempfile::tempdir().unwrap();
        fixture_registry(tmp.path(), "");

        // Requesting the exact version that exists should succeed.
        let consent = resolve_consent_in(tmp.path(), "test-skill", Some("1.0.0")).unwrap();
        assert_eq!(consent.version, "1.0.0");
    }

    #[test]
    fn findings_is_empty_for_clean_skill() {
        let tmp = tempfile::tempdir().unwrap();
        fixture_registry(tmp.path(), "");

        let consent = resolve_consent_in(tmp.path(), "test-skill", None).unwrap();
        assert!(
            consent.findings.is_empty(),
            "clean skill should have no findings"
        );
    }

    #[test]
    fn mcp_requirements_are_formatted() {
        // Skill with one MCP requirement.
        let skill_with_mcp = r#"name: mcp-skill
version: 1.0.0
publisher: human:tester
description: Needs MCP
category: workflow
content:
  abstract: Uses MCP
  context: Requires a browser tool.
mcp_requirements:
  - tool_pattern: browser.*
    capability: network_http
"#;
        let tmp = tempfile::tempdir().unwrap();
        let index_yaml = "skills:\n  mcp-skill:\n    latest: 1.0.0\n    description: Needs MCP\n    publisher: human:tester\n    category: workflow\n    tags: []\n    content_sha256: \"\"\n    install_count: 0\n";
        fs::write(tmp.path().join("index.yaml"), index_yaml).unwrap();
        let versions_dir = tmp.path().join("skills").join("mcp-skill").join("versions");
        fs::create_dir_all(&versions_dir).unwrap();
        fs::write(versions_dir.join("1.0.0.yaml"), skill_with_mcp).unwrap();

        let consent = resolve_consent_in(tmp.path(), "mcp-skill", None).unwrap();
        assert_eq!(consent.mcp_requirements.len(), 1);
        assert!(consent.mcp_requirements[0].contains("browser.*"));
        assert!(consent.mcp_requirements[0].contains("network_http"));
    }
}
