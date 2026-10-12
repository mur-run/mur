//! Tests for the filtered build-cache clone. A plain recursive copy stands in
//! for the COW `cp` so the filter logic runs on every platform.

use super::*;
use std::fs;

fn plain_copy(src: &Path, dst: &Path) -> Result<()> {
    if src.is_dir() {
        fs::create_dir_all(dst)?;
        for e in fs::read_dir(src)? {
            let e = e?;
            plain_copy(&e.path(), &dst.join(e.file_name()))?;
        }
    } else {
        fs::copy(src, dst)?;
    }
    Ok(())
}

fn touch(p: &Path) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, b"x").unwrap();
}

/// `target/{.rustc_info.json, debug/{.fingerprint,deps,build,incremental}/x,
/// debug/mur, tmp/y, turns/turn-z/debug/.fingerprint/w}`
fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::TempDir::new().unwrap();
    let t = tmp.path().join("target");
    touch(&t.join(".rustc_info.json"));
    for d in [".fingerprint", "deps", "build", "incremental"] {
        touch(&t.join("debug").join(d).join("x"));
    }
    touch(&t.join("debug/mur"));
    touch(&t.join("tmp/y"));
    touch(&t.join("turns/turn-z/debug/.fingerprint/w"));
    tmp
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn run(root: &Path, copy: Copier) -> Result<CopyOutcome> {
    let (sub, top) = (strings(&["incremental"]), strings(&["turns"]));
    let plan = CachePlan {
        skip_subdirs: &sub,
        skip_top: &top,
    };
    let src = root.join("target");
    filtered_with(&src, &src.join("turns/turn-a"), &plan, copy)
}

#[test]
fn filtered_copies_deps_build_fingerprint() {
    let tmp = fixture();
    let out = run(tmp.path(), &plain_copy).unwrap();
    assert!(matches!(out, CopyOutcome::Copied { .. }));
    let d = tmp.path().join("target/turns/turn-a");
    for p in [
        ".rustc_info.json",
        "debug/.fingerprint/x",
        "debug/deps/x",
        "debug/build/x",
    ] {
        assert!(d.join(p).is_file(), "missing {p}");
    }
}

#[test]
fn filtered_skips_incremental_profile_binaries_tmp_and_turns() {
    let tmp = fixture();
    run(tmp.path(), &plain_copy).unwrap();
    let d = tmp.path().join("target/turns/turn-a");
    for p in ["debug/incremental", "debug/mur", "tmp", "turns"] {
        assert!(!d.join(p).exists(), "should skip {p}");
    }
}

#[test]
fn filtered_idempotent_on_present_dst() {
    let tmp = fixture();
    run(tmp.path(), &plain_copy).unwrap();
    assert_eq!(
        run(tmp.path(), &plain_copy).unwrap(),
        CopyOutcome::AlreadyPresent
    );
}

#[test]
fn filtered_source_missing() {
    let tmp = tempfile::TempDir::new().unwrap();
    assert_eq!(
        run(tmp.path(), &plain_copy).unwrap(),
        CopyOutcome::SourceMissing
    );
}

#[test]
fn filtered_leaves_no_partial_dst_on_failure() {
    let tmp = fixture();
    let failing = |s: &Path, d: &Path| -> Result<()> {
        if s.ends_with("build") {
            anyhow::bail!("unreadable");
        }
        plain_copy(s, d)
    };
    assert!(run(tmp.path(), &failing).is_err());
    let turns = tmp.path().join("target/turns");
    let names: Vec<_> = fs::read_dir(&turns)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, ["turn-z"], "no dst and no scratch dir left behind");
}

#[cfg(target_os = "macos")]
#[test]
fn filtered_real_cow_clone() {
    let tmp = fixture();
    let (sub, top) = (strings(&["incremental"]), strings(&["turns"]));
    let plan = CachePlan {
        skip_subdirs: &sub,
        skip_top: &top,
    };
    let src = tmp.path().join("target");
    let dst = src.join("turns/turn-a");
    let out = copy_build_cache_filtered(&src, &dst, &plan).unwrap();
    assert!(matches!(out, CopyOutcome::Copied { .. }));
    assert!(dst.join("debug/deps/x").is_file());
    assert!(!dst.join("debug/incremental").exists());
}
