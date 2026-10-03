use super::*;
use std::path::{Path, PathBuf};

#[test]
fn scratch_dir_is_mur_home_tmp_agent() {
    let got = agent_scratch_dir(Path::new("/m/agents/a")).unwrap();
    assert_eq!(got, PathBuf::from("/m/tmp/a"));
}

#[test]
fn root_has_no_mur_home() {
    assert!(matches!(
        agent_scratch_dir(Path::new("/")),
        Err(AgentPathError::NoMurHome(_))
    ));
}

#[test]
fn bare_relative_name_is_an_error() {
    assert!(agent_scratch_dir(Path::new("a")).is_err());
}

#[test]
fn scratch_env_sets_all_three_keys_to_the_dir() {
    let env = scratch_env(Path::new("/m/tmp/a"));
    let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["TMPDIR", "TMP", "TEMP"]);
    assert!(env.iter().all(|(_, v)| v == "/m/tmp/a"));
}

#[cfg(unix)]
fn mode(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[cfg(unix)]
#[test]
fn fresh_scratch_dir_and_parent_are_0700() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("tmp").join("a");
    ensure_scratch_dir(&dir).unwrap();
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&home.path().join("tmp")), 0o700);
}

#[cfg(unix)]
#[test]
fn existing_0755_dirs_are_tightened_to_0700() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let parent = home.path().join("tmp");
    let dir = parent.join("a");
    std::fs::create_dir_all(&dir).unwrap();
    for p in [&parent, &dir] {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    ensure_scratch_dir(&dir).unwrap();
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&parent), 0o700);
}

#[cfg(unix)]
#[test]
fn only_a_wrong_mode_needs_tightening() {
    assert!(!needs_tighten(0o040_700));
    assert!(needs_tighten(0o040_755));
    assert!(needs_tighten(0o040_500));
}

/// #1678: post-seal, `<mur_home>/tmp` is not writable, so a re-run on an
/// already-prepared dir must not `chmod` anything. `chmod` always bumps
/// ctime (even to the same mode), so an unchanged ctime proves no write.
#[cfg(unix)]
#[test]
fn already_prepared_dir_is_not_chmodded_again() {
    use std::os::unix::fs::MetadataExt;
    let home = tempfile::tempdir().unwrap();
    let parent = home.path().join("tmp");
    let dir = parent.join("a");
    ensure_scratch_dir(&dir).unwrap();
    let ctime = |p: &Path| {
        let m = std::fs::metadata(p).unwrap();
        (m.ctime(), m.ctime_nsec())
    };
    let before = (ctime(&parent), ctime(&dir));
    std::thread::sleep(std::time::Duration::from_millis(20));
    ensure_scratch_dir(&dir).unwrap();
    assert_eq!((ctime(&parent), ctime(&dir)), before);
}

mod prune {
    use super::super::prune::{PruneReport, prune_scratch};
    use std::fs;
    use std::time::{Duration, SystemTime};

    const DAY: Duration = Duration::from_secs(86_400);

    /// `now` sits 30 days ahead of the real clock, so anything touched
    /// during the test is "old"; pushing an mtime to `now` makes it fresh.
    fn future_now() -> SystemTime {
        SystemTime::now() + 30 * DAY
    }

    fn make_fresh(p: &std::path::Path, now: SystemTime) {
        fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(now)
            .unwrap();
    }

    /// #8: old file and old symlink go, fresh file stays, link target survives.
    #[cfg(unix)]
    #[test]
    fn removes_old_files_and_links_keeps_fresh_and_targets() {
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let now = future_now();
        let target = outside.path().join("keep.txt");
        fs::write(&target, "x").unwrap();
        fs::write(d.path().join("old.txt"), "o").unwrap();
        fs::write(d.path().join("fresh.txt"), "f").unwrap();
        make_fresh(&d.path().join("fresh.txt"), now);
        std::os::unix::fs::symlink(&target, d.path().join("link")).unwrap();

        let r = prune_scratch(d.path(), 7 * DAY, now);

        assert_eq!(
            r,
            PruneReport {
                removed: 2,
                kept: 1,
                errors: 0
            }
        );
        assert!(!d.path().join("old.txt").exists());
        assert!(d.path().join("fresh.txt").exists());
        assert!(fs::symlink_metadata(d.path().join("link")).is_err());
        assert!(target.exists(), "link target outside scratch must survive");
    }

    /// #8b: newest mtime anywhere in the tree decides; the root is kept.
    #[test]
    fn nested_tree_age_is_its_newest_entry() {
        let d = tempfile::tempdir().unwrap();
        let now = future_now();
        let live = d.path().join("target/debug/deps");
        fs::create_dir_all(&live).unwrap();
        fs::write(live.join("hot.o"), "h").unwrap();
        fs::write(d.path().join("target/cold.txt"), "c").unwrap();
        make_fresh(&live.join("hot.o"), now);
        let stale = d.path().join("stale/a/b");
        fs::create_dir_all(&stale).unwrap();
        fs::write(stale.join("x"), "x").unwrap();

        let r = prune_scratch(d.path(), 7 * DAY, now);

        assert_eq!(
            r,
            PruneReport {
                removed: 1,
                kept: 1,
                errors: 0
            }
        );
        assert!(
            d.path().join("target/cold.txt").exists(),
            "no partial prune"
        );
        assert!(live.join("hot.o").exists());
        assert!(!d.path().join("stale").exists());
        assert!(d.path().exists());
    }

    /// One unreadable entry counts as an error and the pass continues.
    #[cfg(unix)]
    #[test]
    fn per_entry_error_is_counted_and_pass_continues() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let bad = d.path().join("bad");
        fs::create_dir_all(bad.join("inner")).unwrap();
        fs::write(d.path().join("old.txt"), "o").unwrap();
        fs::set_permissions(&bad, fs::Permissions::from_mode(0o000)).unwrap();

        let r = prune_scratch(d.path(), 7 * DAY, future_now());

        fs::set_permissions(&bad, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(r.errors, 1);
        assert_eq!(r.removed, 1);
        assert!(bad.exists(), "an entry whose age is unknown is kept");
        assert!(!d.path().join("old.txt").exists());
    }

    #[test]
    fn missing_dir_is_one_error_not_a_panic() {
        let d = tempfile::tempdir().unwrap();
        let r = prune_scratch(&d.path().join("nope"), DAY, SystemTime::now());
        assert_eq!(
            r,
            PruneReport {
                removed: 0,
                kept: 0,
                errors: 1
            }
        );
    }
}
