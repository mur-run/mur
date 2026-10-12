use super::*;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

const TRACK: &str = "turn-a";
const PLENTY: u64 = 1 << 50;

fn fake_clone(_src: &Path, dst: &Path, _plan: &CachePlan) -> anyhow::Result<CopyOutcome> {
    fs::create_dir_all(dst.join("debug/deps"))?;
    Ok(CopyOutcome::Copied {
        elapsed: Duration::from_millis(1),
    })
}

fn plenty(_: &Path) -> io::Result<u64> {
    Ok(PLENTY)
}

fn scarce(_: &Path) -> io::Result<u64> {
    Ok(0)
}

fn project() -> tempfile::TempDir {
    let tmp = tempfile::TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join("target/debug/.fingerprint")).unwrap();
    tmp
}

fn dst(p: &Path) -> PathBuf {
    turn_target_dir(
        p,
        &WarmBuildCacheConfig::default().turn_target_subdir,
        TRACK,
    )
}

fn warm(p: &Path, cfg: &WarmBuildCacheConfig, free: FreeSpace) -> WarmOutcome {
    let track = git_track();
    warm_with(p, track.path(), TRACK, cfg, free, fake_clone)
}

fn git_in(dir: &Path, args: &[&str]) {
    let st = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?} failed");
}

/// A git worktree standing in for the turn track.
fn git_track() -> tempfile::TempDir {
    let t = tempfile::TempDir::new().unwrap();
    git_in(t.path(), &["init", "-q"]);
    t
}

/// An hour ago: older than any target the clone could have brought in.
fn age(path: &Path) {
    let old = std::time::SystemTime::now() - Duration::from_secs(3600);
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(old)
        .unwrap();
}

fn mtime(path: &Path) -> std::time::SystemTime {
    fs::metadata(path).unwrap().modified().unwrap()
}

#[test]
fn disabled() {
    let p = project();
    let cfg = WarmBuildCacheConfig {
        enabled: false,
        ..Default::default()
    };
    assert_eq!(
        warm(p.path(), &cfg, plenty),
        WarmOutcome::Skipped(SkipReason::Disabled)
    );
    assert!(!dst(p.path()).exists());
}

#[test]
fn already_warm() {
    let p = project();
    fs::create_dir_all(dst(p.path())).unwrap();
    assert_eq!(
        warm(p.path(), &Default::default(), plenty),
        WarmOutcome::AlreadyWarm
    );
}

#[test]
fn low_disk_skips_without_touching_dst() {
    let p = project();
    let out = warm(p.path(), &Default::default(), scarce);
    assert!(matches!(
        out,
        WarmOutcome::Skipped(SkipReason::LowDisk { free_gib: 0, .. })
    ));
    assert!(!dst(p.path()).exists());
}

#[test]
fn sweep_runs_before_floor() {
    let p = project();
    let orphan = turn_target_dir(p.path(), "turns", "turn-gone");
    fs::create_dir_all(&orphan).unwrap();
    warm(p.path(), &Default::default(), scarce);
    assert!(
        !orphan.exists(),
        "orphan must be swept even when disk is low"
    );
}

#[test]
fn warms_into_turn_dir() {
    let p = project();
    let out = warm(p.path(), &Default::default(), plenty);
    assert!(matches!(out, WarmOutcome::Warmed { .. }));
    assert!(dst(p.path()).join("debug/deps").is_dir());
}

#[test]
fn no_source_target() {
    let p = tempfile::TempDir::new().unwrap();
    assert_eq!(
        warm(p.path(), &Default::default(), plenty),
        WarmOutcome::Skipped(SkipReason::NoSourceTarget)
    );
}

/// The bug: a source file older than the main target's build outputs was
/// treated as fresh, so the clone silently reused a stale build script.
#[test]
fn warm_bumps_track_sources_past_the_cloned_target() {
    let p = project();
    let track = git_track();
    let tracked = track.path().join("build.rs");
    let untracked = track.path().join("new.rs");
    fs::write(&tracked, "fn main() {}").unwrap();
    git_in(track.path(), &["add", "build.rs"]);
    fs::write(&untracked, "").unwrap();
    age(&tracked);
    age(&untracked);
    let before = std::time::SystemTime::now() - Duration::from_secs(60);

    let out = warm_with(
        p.path(),
        track.path(),
        TRACK,
        &Default::default(),
        plenty,
        fake_clone,
    );

    assert!(matches!(out, WarmOutcome::Warmed { .. }));
    assert!(mtime(&tracked) > before, "tracked source left stale");
    assert!(mtime(&untracked) > before, "untracked source left stale");
}

#[test]
fn warm_skips_and_drops_clone_when_sources_cannot_be_listed() {
    let p = project();
    let not_git = tempfile::TempDir::new().unwrap();
    let out = warm_with(
        p.path(),
        not_git.path(),
        TRACK,
        &Default::default(),
        plenty,
        fake_clone,
    );
    assert!(matches!(out, WarmOutcome::Skipped(SkipReason::Failed(_))));
    assert!(
        !dst(p.path()).exists(),
        "a clone that may be stale must not stay"
    );
}
