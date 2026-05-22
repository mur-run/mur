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

/// Flip a single byte deep inside the gzip stream — the tarball CRC or
/// any signature check must catch this.
#[test]
fn tarball_content_tamper_causes_refuse() {
    let (_tmp, out) = make_test_package();
    let mut data = std::fs::read(&out).unwrap();
    if data.len() > 100 {
        let mid = data.len() / 2;
        data[mid] ^= 0xFF;
    }
    std::fs::write(&out, &data).unwrap();

    match MuragentArchive::read(&out) {
        Err(_) => {} // CRC failed → fatal, as expected
        Ok(archive) => {
            assert!(
                validator::validate(&archive).is_err(),
                "tampered content must be rejected by validate"
            );
        }
    }
}

/// Re-pack a valid muragent without the signature, then ensure validation
/// refuses it. This proves that absence of signatures.json is fatal.
#[test]
fn missing_signatures_json_causes_refuse() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::fs;
    use tar::Builder;

    let (_tmp, original) = make_test_package();
    let archive = MuragentArchive::read(&original).unwrap();

    // Rebuild the tar.gz, stripping signatures.json
    let stripped_path = original.with_file_name("stripped.muragent");
    {
        let f = fs::File::create(&stripped_path).unwrap();
        let gz = GzEncoder::new(f, Compression::default());
        let mut tar = Builder::new(gz);
        for (path, data) in &archive.files {
            if path == "signatures.json" {
                continue;
            }
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, path, data.as_slice()).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();
    }

    let stripped = MuragentArchive::read(&stripped_path).unwrap();
    assert!(
        validator::validate(&stripped).is_err(),
        "missing signatures.json must be rejected"
    );
}

/// Same idea but for manifest.signed.json — its absence must be fatal.
#[test]
fn missing_signed_json_causes_refuse() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::fs;
    use tar::Builder;

    let (_tmp, original) = make_test_package();
    let archive = MuragentArchive::read(&original).unwrap();

    let stripped_path = original.with_file_name("nosigned.muragent");
    {
        let f = fs::File::create(&stripped_path).unwrap();
        let gz = GzEncoder::new(f, Compression::default());
        let mut tar = Builder::new(gz);
        for (path, data) in &archive.files {
            if path == "manifest.signed.json" {
                continue;
            }
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, path, data.as_slice()).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();
    }

    let stripped = MuragentArchive::read(&stripped_path).unwrap();
    assert!(
        validator::validate(&stripped).is_err(),
        "missing manifest.signed.json must be rejected"
    );
}
