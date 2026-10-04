use super::*;
use std::io::Write;

fn sha(b: &[u8]) -> String {
    sha256_hex(b)
}

fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut w = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap();
    }
    buf.into_inner()
}

const BIN: &[u8] = b"#!/bin/sh\necho ast-grep 0.45.3\n";

fn pinned_for(zip: &[u8], member: &'static str, bin: &[u8]) -> Pinned {
    Pinned {
        target: "test-target",
        zip_sha256: sha(zip),
        member,
        bin_sha256: sha(bin),
    }
}

fn dest(home: &tempfile::TempDir) -> PathBuf {
    mur_common::config::ast_grep_binary_path(home.path())
}

#[test]
fn supported_platforms_have_pins() {
    for (os, arch, member) in [
        ("macos", "aarch64", "ast-grep"),
        ("linux", "x86_64", "ast-grep"),
        ("windows", "x86_64", "ast-grep.exe"),
    ] {
        let p = pinned_for_platform(os, arch).unwrap_or_else(|| panic!("{os}/{arch}"));
        assert_eq!(p.member, member);
        assert_eq!(p.zip_sha256.len(), 64);
        assert_eq!(p.bin_sha256.len(), 64);
    }
}

#[test]
fn unsupported_platform_has_no_pin() {
    // MUR ships Apple Silicon only on macOS; no unverified fallback.
    assert!(pinned_for_platform("macos", "x86_64").is_none());
    assert!(pinned_for_platform("linux", "aarch64").is_none());
}

#[test]
fn download_url_is_the_pinned_release_asset() {
    let p = pinned_for_platform("linux", "x86_64").unwrap();
    assert_eq!(
        download_url(&p),
        format!(
            "https://github.com/ast-grep/ast-grep/releases/download/{}/app-x86_64-unknown-linux-gnu.zip",
            mur_common::config::AST_GREP_PINNED_VERSION
        )
    );
}

#[test]
fn installs_verified_binary_at_resolver_path() {
    let home = tempfile::tempdir().unwrap();
    let zip = zip_of(&[("sg", b"alias"), ("ast-grep", BIN)]);
    let p = pinned_for(&zip, "ast-grep", BIN);
    let out = verify_and_place(&zip, &p, &dest(&home)).unwrap();
    assert_eq!(out, Outcome::Installed);
    assert_eq!(std::fs::read(dest(&home)).unwrap(), BIN);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dest(&home)).unwrap().permissions().mode();
        assert_ne!(mode & 0o111, 0, "must be executable");
    }
    // The `sg` alias is not installed: only the pinned member is placed.
    assert!(!dest(&home).with_file_name("sg").exists());
}

#[test]
fn zip_sha_mismatch_fails_closed() {
    let home = tempfile::tempdir().unwrap();
    let zip = zip_of(&[("ast-grep", BIN)]);
    let mut p = pinned_for(&zip, "ast-grep", BIN);
    p.zip_sha256 = "0".repeat(64);
    let e = verify_and_place(&zip, &p, &dest(&home))
        .unwrap_err()
        .to_string();
    assert!(e.contains("sha256 mismatch"), "{e}");
    assert!(!dest(&home).exists());
    assert!(!dest(&home).parent().unwrap().exists(), "nothing created");
}

#[test]
fn binary_sha_mismatch_fails_closed() {
    let home = tempfile::tempdir().unwrap();
    let zip = zip_of(&[("ast-grep", BIN)]);
    let p = pinned_for(&zip, "ast-grep", b"some other binary");
    let e = verify_and_place(&zip, &p, &dest(&home))
        .unwrap_err()
        .to_string();
    assert!(e.contains("binary sha256 mismatch"), "{e}");
    assert!(!dest(&home).exists());
}

#[test]
fn missing_member_fails_closed() {
    let home = tempfile::tempdir().unwrap();
    let zip = zip_of(&[("sg", BIN)]);
    let p = pinned_for(&zip, "ast-grep", BIN);
    let e = verify_and_place(&zip, &p, &dest(&home))
        .unwrap_err()
        .to_string();
    assert!(e.contains("ast-grep"), "{e}");
    assert!(!dest(&home).exists());
}

#[test]
fn nested_entry_with_same_basename_is_not_the_member() {
    let home = tempfile::tempdir().unwrap();
    let zip = zip_of(&[("evil/ast-grep", BIN)]);
    let p = pinned_for(&zip, "ast-grep", BIN);
    assert!(verify_and_place(&zip, &p, &dest(&home)).is_err());
    assert!(!dest(&home).exists());
}

#[test]
fn verified_existing_binary_is_left_alone() {
    let home = tempfile::tempdir().unwrap();
    let zip = zip_of(&[("ast-grep", BIN)]);
    let p = pinned_for(&zip, "ast-grep", BIN);
    verify_and_place(&zip, &p, &dest(&home)).unwrap();
    assert_eq!(installed_state(&dest(&home), &p), Installed::Verified);
    assert_eq!(
        verify_and_place(&zip, &p, &dest(&home)).unwrap(),
        Outcome::AlreadyInstalled
    );
}

#[test]
fn tampered_existing_binary_is_replaced() {
    let home = tempfile::tempdir().unwrap();
    let zip = zip_of(&[("ast-grep", BIN)]);
    let p = pinned_for(&zip, "ast-grep", BIN);
    std::fs::create_dir_all(dest(&home).parent().unwrap()).unwrap();
    std::fs::write(dest(&home), b"tampered").unwrap();
    assert_eq!(installed_state(&dest(&home), &p), Installed::Mismatch);
    assert_eq!(
        verify_and_place(&zip, &p, &dest(&home)).unwrap(),
        Outcome::Installed
    );
    assert_eq!(std::fs::read(dest(&home)).unwrap(), BIN);
}

#[test]
fn absent_binary_state_is_missing() {
    let home = tempfile::tempdir().unwrap();
    let p = pinned_for_platform("linux", "x86_64").unwrap();
    assert_eq!(installed_state(&dest(&home), &p), Installed::Missing);
}

#[test]
fn zip_pins_match_ci_workflow() {
    // CI installs the same zips with its own sha table; the two must agree.
    let ci = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.github/workflows/ci.yml");
    let Ok(ci) = std::fs::read_to_string(ci) else {
        return; // packaged crate: no workflow file to compare against
    };
    for (os, arch) in [
        ("macos", "aarch64"),
        ("linux", "x86_64"),
        ("windows", "x86_64"),
    ] {
        let p = pinned_for_platform(os, arch).unwrap();
        assert!(
            ci.contains(&format!("sha={}", p.zip_sha256)),
            "{os}/{arch} zip sha drifted from ci.yml"
        );
    }
}

#[test]
#[ignore = "network: downloads the real pinned release"]
fn live_install_into_scratch_home() {
    let home = std::env::var_os("MUR_LIVE_HOME")
        .map(PathBuf::from)
        .unwrap();
    let d = mur_common::config::ast_grep_binary_path(&home);
    assert_eq!(install(&d).unwrap(), Outcome::Installed);
    assert_eq!(install(&d).unwrap(), Outcome::AlreadyInstalled);
}
