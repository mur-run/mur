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
    warm_with(p, TRACK, cfg, free, fake_clone)
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
