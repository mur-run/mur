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
fn model_requirements_schema_contract_is_enforced() {
    use mur_common::muragent::manifest::{ModelRequirements, ModelTier};
    use mur_common::muragent::writer::build_manifest_from_profile_with_requirements;

    let tmp = TempDir::new().unwrap();
    let profile = AgentProfile::default_for_tests();
    let requirements = ModelRequirements {
        chat: true,
        tools: true,
        minimum_context_window: None,
    };

    let cases = [
        ("v3-valid", "mur-agent/3", true, false, true),
        ("v2-with-requirements", "mur-agent/2", true, false, false),
        (
            "v3-without-requirements",
            "mur-agent/3",
            false,
            false,
            false,
        ),
        ("v3-with-model-hint", "mur-agent/3", true, true, false),
        ("unknown-schema", "mur-agent/99", false, false, false),
    ];

    for (name, schema, has_requirements, has_hint, should_validate) in cases {
        let out = tmp.path().join(format!("{name}.muragent"));
        let mut manifest = build_manifest_from_profile_with_requirements(
            &profile,
            "2.85.0",
            has_requirements.then(|| requirements.clone()),
        );
        manifest.schema = schema.into();
        if has_hint {
            manifest.model_hint = Some(mur_common::muragent::manifest::ModelHint {
                provider: "ollama".into(),
                name: "legacy".into(),
                tier: ModelTier::Small,
                min_ram_gb: 0,
                local_capable: true,
            });
        }
        let writer = MuragentWriter::new(
            manifest,
            serde_yaml_ng::to_string(&profile).unwrap(),
            AgentIdentity::generate(),
        );
        writer.write(&out).unwrap();
        let archive = MuragentArchive::read(&out).unwrap();
        assert_eq!(
            validator::validate(&archive).is_ok(),
            should_validate,
            "{name}"
        );
    }
}

#[test]
fn signed_model_requirements_reject_tampering() {
    use mur_common::muragent::manifest::ModelRequirements;
    use mur_common::muragent::writer::build_manifest_from_profile_with_requirements;

    let tmp = TempDir::new().unwrap();
    let original = tmp.path().join("signed-v3.muragent");
    let tampered = tmp.path().join("tampered-v3.muragent");
    let profile = AgentProfile::default_for_tests();
    let manifest = build_manifest_from_profile_with_requirements(
        &profile,
        "2.85.0",
        Some(ModelRequirements {
            chat: true,
            tools: true,
            minimum_context_window: None,
        }),
    );
    MuragentWriter::new(
        manifest,
        serde_yaml_ng::to_string(&profile).unwrap(),
        AgentIdentity::generate(),
    )
    .write(&original)
    .unwrap();

    let archive = MuragentArchive::read(&original).unwrap();
    let mut files = archive.files_as_vec();
    let manifest = files
        .iter_mut()
        .find(|(path, _)| path == "manifest.yaml")
        .unwrap();
    let text = String::from_utf8(manifest.1.clone()).unwrap();
    manifest.1 = text.replace("tools: true", "tools: false").into_bytes();
    write_test_archive(&tampered, files);

    let archive = MuragentArchive::read(&tampered).unwrap();
    assert!(validator::validate(&archive).is_err());
}

fn write_test_archive(path: &std::path::Path, files: Vec<(String, Vec<u8>)>) {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::fs::File;
    use tar::Builder;

    let mut tar = Builder::new(GzEncoder::new(
        File::create(path).unwrap(),
        Compression::default(),
    ));
    for (name, bytes) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, name, bytes.as_slice())
            .unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap();
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
