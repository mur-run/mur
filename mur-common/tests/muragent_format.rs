//! Integration test: full export → validate roundtrip for `.muragent` v2.

use mur_common::agent::AgentProfile;
use mur_common::identity::AgentIdentity;
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

    let archive = MuragentArchive::read(&out).unwrap();
    let result = validator::validate(&archive).unwrap();

    assert_eq!(result.manifest.schema, "mur-agent/2");
    assert_eq!(result.manifest.agent.slug, profile.name);
}

#[test]
fn legacy_schema_rejected() {
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
    assert!(
        err.contains("mur-agent/2"),
        "error should mention expected version, got: {err}"
    );
}

#[test]
fn bundle_id_mismatch_rejected() {
    let tmp = TempDir::new().unwrap();
    let out = tmp.path().join("bad-bundle.muragent");

    let profile = AgentProfile::default_for_tests();
    let identity = AgentIdentity::generate();
    let mut manifest = build_manifest_from_profile(&profile, "2.13.0");
    manifest.agent.bundle_id = "io.example.evil".into();

    let profile_yaml = serde_yaml_ng::to_string(&profile).unwrap();
    let mut writer = MuragentWriter::new(manifest, profile_yaml, identity);
    writer.add_icon("icon-512.png", b"data".to_vec());
    writer.write(&out).unwrap();

    let archive = MuragentArchive::read(&out).unwrap();
    assert!(validator::validate(&archive).is_err());
}
