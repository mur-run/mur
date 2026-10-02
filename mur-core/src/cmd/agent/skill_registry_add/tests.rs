use super::*;
use mur_common::skill::publisher_trust::PublisherKeyring;
use std::fs;

// ── fixture helpers ─────────────────────────────────────────────────

/// Keyring with no trusted or revoked entries (all signers → Unsigned/Untrusted).
fn empty_keyring() -> PublisherKeyring {
    PublisherKeyring {
        schema_version: 1,
        publishers: vec![],
        revoked: vec![],
    }
}

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

    let consent = resolve_consent_in(tmp.path(), "test-skill", None, &empty_keyring()).unwrap();

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

    let consent = resolve_consent_in(tmp.path(), "test-skill", None, &empty_keyring()).unwrap();

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

    let consent = resolve_consent_in(tmp.path(), "test-skill", None, &empty_keyring()).unwrap();

    assert_eq!(consent.hash, "mismatch");
    assert!(consent.blocking, "mismatch must be blocking");
    assert!(!consent.needs_ack, "blocking overrides needs_ack");
}

#[test]
fn skill_not_in_index_returns_error() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("index.yaml"), "skills: {}\n").unwrap();

    let err = resolve_consent_in(tmp.path(), "nonexistent", None, &empty_keyring()).unwrap_err();
    assert!(err.to_string().contains("not found in registry"));
}

#[test]
fn explicit_version_not_available_returns_error() {
    let tmp = tempfile::tempdir().unwrap();
    // Index says latest=1.0.0; file only has 1.0.0.yaml.
    fixture_registry(tmp.path(), "");

    // Request a version that doesn't exist and is not `latest`.
    let err =
        resolve_consent_in(tmp.path(), "test-skill", Some("9.9.9"), &empty_keyring()).unwrap_err();
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
    let consent =
        resolve_consent_in(tmp.path(), "test-skill", Some("1.0.0"), &empty_keyring()).unwrap();
    assert_eq!(consent.version, "1.0.0");
}

#[test]
fn findings_is_empty_for_clean_skill() {
    let tmp = tempfile::tempdir().unwrap();
    fixture_registry(tmp.path(), "");

    let consent = resolve_consent_in(tmp.path(), "test-skill", None, &empty_keyring()).unwrap();
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

    let consent = resolve_consent_in(tmp.path(), "mcp-skill", None, &empty_keyring()).unwrap();
    assert_eq!(consent.mcp_requirements.len(), 1);
    assert!(consent.mcp_requirements[0].contains("browser.*"));
    assert!(consent.mcp_requirements[0].contains("network_http"));
}

// Fix D: path-sanitization tests
#[test]
fn traversal_skill_name_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    fixture_registry(tmp.path(), "");

    let err = resolve_consent_in(tmp.path(), "../evil", None, &empty_keyring()).unwrap_err();
    assert!(
        err.to_string().contains("invalid skill name"),
        "unexpected: {err}"
    );
}

#[test]
fn traversal_version_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    fixture_registry(tmp.path(), "");

    let err = resolve_consent_in(tmp.path(), "test-skill", Some("../evil"), &empty_keyring())
        .unwrap_err();
    assert!(
        err.to_string().contains("invalid version"),
        "unexpected: {err}"
    );
}

// Fix E: gate() unit tests
fn clean_consent() -> ConsentInfo {
    ConsentInfo {
        name: "foo".into(),
        version: "1.0.0".into(),
        publisher: "human:tester".into(),
        category: "context".into(),
        signature: SigView {
            status: "verified".into(),
            publisher: "tester".into(),
            key_fp: "abc".into(),
        },
        hash: "match".into(),
        mcp_requirements: vec![],
        findings: vec![],
        blocking: false,
        needs_ack: false,
        scan_blocking: false,
        trust_level: "sandboxed".into(),
        signer_trust: "trusted".into(),
        body: "".into(),
        resolved_sha256: "aabbcc".into(),
        trust_sha256: "ddeeff".into(),
        drift: None,
    }
}

#[test]
fn gate_blocking_unconditional_even_with_accept() {
    let consent = ConsentInfo {
        blocking: true,
        ..clean_consent()
    };
    // accept=true must NOT override verify-blocking
    assert!(gate(&consent, true).is_err());
    assert!(gate(&consent, false).is_err());
}

#[test]
fn gate_needs_ack_requires_accept() {
    let consent = ConsentInfo {
        needs_ack: true,
        ..clean_consent()
    };
    assert!(gate(&consent, false).is_err());
    assert!(gate(&consent, true).is_ok());
}

#[test]
fn gate_scan_blocking_requires_accept() {
    let consent = ConsentInfo {
        scan_blocking: true,
        ..clean_consent()
    };
    assert!(gate(&consent, false).is_err());
    assert!(gate(&consent, true).is_ok());
}

#[test]
fn gate_all_clean_always_ok() {
    let consent = clean_consent();
    assert!(gate(&consent, false).is_ok());
    assert!(gate(&consent, true).is_ok());
}

// ── I3: signer-trust fold tests (signed skill fixture) ─────────────────

/// Build a signed skill YAML fixture under `dir` (index + 1.0.0.yaml).
/// Returns the `key_fp` (DSSE `keyid`) for use in keyring construction.
fn fixture_registry_signed(dir: &std::path::Path) -> String {
    use mur_common::identity::AgentIdentity;
    use mur_common::muragent::dsse::DsseEnvelope;
    use mur_common::skill::{Skill, TrustLevel, parse_canonical, sign::sign_manifest};

    let id = AgentIdentity::generate();
    let m = parse_canonical(SKILL_YAML).unwrap();
    let env_json = sign_manifest(&m, &id).unwrap();

    // Extract key_fp from the DSSE envelope signatures[0].keyid.
    let envelope: DsseEnvelope = serde_json::from_str(&env_json).unwrap();
    let key_fp = envelope.signatures.first().unwrap().keyid.clone();

    // Serialize the full Skill struct (manifest + publisher_signature).
    let skill = Skill {
        manifest: m,
        content_sha256: None,
        trust_level: TrustLevel::Sandboxed,
        capabilities_declared: vec![],
        publisher_signature: Some(env_json),
    };
    let skill_yaml = serde_yaml_ng::to_string(&skill).unwrap();
    let sha = sha256_hex(&skill_yaml);

    let index_yaml = format!(
        "skills:\n  test-skill:\n    latest: 1.0.0\n    description: test skill\n    publisher: human:tester\n    category: context\n    tags: []\n    content_sha256: \"{sha}\"\n    install_count: 0\n"
    );
    fs::write(dir.join("index.yaml"), &index_yaml).unwrap();
    let versions_dir = dir.join("skills").join("test-skill").join("versions");
    fs::create_dir_all(&versions_dir).unwrap();
    fs::write(versions_dir.join("1.0.0.yaml"), &skill_yaml).unwrap();

    key_fp
}

#[test]
fn signer_trusted_in_keyring_is_clean() {
    use mur_common::skill::publisher_trust::TrustedPublisher;

    let tmp = tempfile::tempdir().unwrap();
    let key_fp = fixture_registry_signed(tmp.path());

    let keyring = PublisherKeyring {
        schema_version: 1,
        publishers: vec![TrustedPublisher {
            name: "tester".into(),
            key_fp: key_fp.clone(),
            comment: String::new(),
        }],
        revoked: vec![],
    };

    let consent = resolve_consent_in(tmp.path(), "test-skill", None, &keyring).unwrap();
    assert!(!consent.blocking, "trusted+hash-match must not be blocking");
    assert!(!consent.needs_ack, "trusted+verified must not need ack");
    assert_eq!(consent.signer_trust, "trusted");
}

#[test]
fn signer_revoked_is_blocking() {
    let tmp = tempfile::tempdir().unwrap();
    let key_fp = fixture_registry_signed(tmp.path());

    let keyring = PublisherKeyring {
        schema_version: 1,
        publishers: vec![],
        revoked: vec![key_fp],
    };

    let consent = resolve_consent_in(tmp.path(), "test-skill", None, &keyring).unwrap();
    assert!(consent.blocking, "revoked signer must be blocking");
    assert_eq!(consent.signer_trust, "revoked");
}

#[test]
fn signer_unknown_key_needs_ack() {
    let tmp = tempfile::tempdir().unwrap();
    fixture_registry_signed(tmp.path()); // key_fp discarded — not in keyring

    // Empty keyring → signer is Untrusted (valid sig but unknown key).
    let consent = resolve_consent_in(tmp.path(), "test-skill", None, &empty_keyring()).unwrap();
    assert!(
        !consent.blocking,
        "unknown signer must not be hard-blocking"
    );
    assert!(consent.needs_ack, "unknown signer must require ack");
    assert_eq!(consent.signer_trust, "untrusted");
}

// ── C1 regression: drift lookup must use name-key, not hash-key ─────────

#[test]
fn c1_drift_lookup_uses_name_key_not_hash_key() {
    use mur_common::skill::TrustLevel;
    use mur_common::trust::skills::{SkillTrustStore, TrustEntry};

    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path();

    // Insert TWO entries for "test-skill":
    //  - hash-keyed (as written by `mur skill install`): key = 64-hex chars,
    //    content_sha256 is empty. If returned, check_drift skips comparison → None.
    //  - name-keyed (as written by cmd_skill_registry_add): key = "test-skill",
    //    content_sha256 = "old_hash_value". If returned, drift is detected.
    //
    // In a BTreeMap, "a".repeat(64) sorts before "test-skill" (ASCII 'a' < 't'),
    // so the buggy .values().find() returns the hash-keyed entry first → silent fail.
    let mut ts = SkillTrustStore::default();
    let hash_key = "a".repeat(64);
    ts.entries.insert(
        hash_key,
        TrustEntry {
            name: "test-skill".into(),
            version: "1.0.0".into(),
            level: TrustLevel::Sandboxed,
            installed_at: "2026-01-01T00:00:00Z".into(),
            publisher: None,
            content_sha256: String::new(), // empty — would cause check_drift to skip
            signer_key_fp: None,
        },
    );
    ts.entries.insert(
        "test-skill".into(),
        TrustEntry {
            name: "test-skill".into(),
            version: "1.0.0".into(),
            level: TrustLevel::Sandboxed,
            installed_at: "2026-01-01T00:00:00Z".into(),
            publisher: None,
            content_sha256: "old_hash_value".into(), // real pin
            signer_key_fp: None,
        },
    );
    ts.save(mur_home).unwrap();

    // drift_status uses entries.get("test-skill") → name-keyed entry →
    // "old_hash_value" vs "new_different_hash" → DriftDecision::Changed.
    let (desc, decision) =
        drift_status(mur_home, "test-skill", "new_different_hash", None, "1.0.0");
    assert!(
        desc.is_some(),
        "C1 regression: name-keyed entry must be found so drift is detected (not silently None)"
    );
    assert!(
        matches!(decision, DriftDecision::Changed { .. }),
        "C1 regression: expected Changed, got {decision:?}"
    );
}
