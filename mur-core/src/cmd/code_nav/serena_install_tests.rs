use super::*;
use crate::cmd::code_nav::plan::{self, Detected, Flags};
use std::collections::BTreeSet;
use std::ffi::OsStr;

/// Lay down what `uv tool install` leaves behind: the entry point and the
/// dist-info carrying `direct_url.json`.
fn fake_install(dir: &Path, version: &str, commit: &str) {
    let site = dir
        .join(UV_TOOL_SUBDIR)
        .join(PACKAGE)
        .join("lib/python3.12/site-packages")
        .join(format!("serena_agent-{version}.dist-info"));
    std::fs::create_dir_all(&site).unwrap();
    std::fs::write(
        site.join("direct_url.json"),
        format!(
            r#"{{"url":"{SERENA_GIT_URL}","vcs_info":{{"vcs":"git","commit_id":"{commit}"}}}}"#
        ),
    )
    .unwrap();
    let bin = serena_binary_path_in(dir);
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
}

fn env_of(cmd: &std::process::Command, key: &str) -> Option<PathBuf> {
    cmd.get_envs()
        .find(|(k, _)| *k == OsStr::new(key))
        .and_then(|(_, v)| v.map(PathBuf::from))
}

#[test]
fn dir_matches_the_planner_install_row() {
    let home = Path::new("/h");
    let p = plan::plan(
        home,
        &Flags {
            with_serena: true,
            ..Default::default()
        },
        &Detected {
            on_path: BTreeSet::from(["uv".to_string()]),
        },
    )
    .unwrap();
    let row = p.install.iter().find(|r| r.name == plan::SERENA).unwrap();
    assert_eq!(serena_dir(home), row.dir);
}

#[test]
fn command_pins_commit_and_dependency_cutoff() {
    let dir = Path::new("/h/tools/serena/2.0.0.dev0");
    let cmd = install_command(Path::new("uv"), dir, false);
    let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy()).collect();
    assert_eq!(&args[..2], ["tool", "install"]);
    assert!(args.iter().any(|a| a == "--no-config"), "{args:?}");
    let cut = args.iter().position(|a| a == "--exclude-newer").unwrap();
    assert_eq!(args[cut + 1], SERENA_EXCLUDE_NEWER);
    let spec = args.last().unwrap();
    assert_eq!(
        spec.as_ref(),
        format!("{PACKAGE} @ git+{SERENA_GIT_URL}@{SERENA_GIT_REV}")
    );
    assert_eq!(SERENA_GIT_REV.len(), 40, "full commit sha, not a branch");
    assert!(!args.iter().any(|a| a == "--reinstall"));
}

#[test]
fn reinstall_flag_only_when_asked() {
    let cmd = install_command(Path::new("uv"), Path::new("/d"), true);
    assert!(cmd.get_args().any(|a| a == "--reinstall"));
}

#[test]
fn command_confines_uv_to_the_managed_dir() {
    let dir = Path::new("/h/tools/serena/2.0.0.dev0");
    let cmd = install_command(Path::new("uv"), dir, false);
    assert_eq!(env_of(&cmd, "UV_TOOL_DIR"), Some(dir.join(UV_TOOL_SUBDIR)));
    assert_eq!(env_of(&cmd, "UV_TOOL_BIN_DIR"), Some(dir.join(BIN_SUBDIR)));
    assert_eq!(cmd.get_current_dir(), Some(dir), "no repo pyproject in cwd");
}

#[test]
fn verified_only_for_the_pinned_version_and_commit() {
    let t = tempfile::tempdir().unwrap();
    assert_eq!(installed_state(t.path()), Installed::Missing);
    fake_install(t.path(), plan::SERENA_PIN, SERENA_GIT_REV);
    assert_eq!(installed_state(t.path()), Installed::Verified);
}

#[test]
fn other_commit_is_a_mismatch() {
    let t = tempfile::tempdir().unwrap();
    fake_install(t.path(), plan::SERENA_PIN, &"0".repeat(40));
    assert_eq!(installed_state(t.path()), Installed::Mismatch);
}

#[test]
fn other_version_is_a_mismatch() {
    let t = tempfile::tempdir().unwrap();
    fake_install(t.path(), "1.7.0", SERENA_GIT_REV);
    assert_eq!(installed_state(t.path()), Installed::Mismatch);
}

#[test]
fn missing_entry_point_is_not_verified() {
    let t = tempfile::tempdir().unwrap();
    fake_install(t.path(), plan::SERENA_PIN, SERENA_GIT_REV);
    std::fs::remove_file(serena_binary_path_in(t.path())).unwrap();
    assert_eq!(installed_state(t.path()), Installed::Missing);
}

#[test]
fn record_carries_the_full_pin() {
    let r = Record::for_dir(Path::new("/d"));
    assert_eq!(r.version, plan::SERENA_PIN);
    assert_eq!(r.git_rev, SERENA_GIT_REV);
    assert_eq!(r.exclude_newer, SERENA_EXCLUDE_NEWER);
    assert_eq!(r.bin, serena_binary_path_in(Path::new("/d")));
    let j = serde_json::to_value(&r).unwrap();
    assert_eq!(j["git_rev"], SERENA_GIT_REV);
}

#[cfg(unix)]
mod with_fake_uv {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A `uv` stand-in that installs `commit`, and logs each call.
    fn fake_uv(t: &Path, commit: &str, exit: i32) -> PathBuf {
        let uv = t.join("fake-uv");
        let site = format!(
            "$UV_TOOL_DIR/{PACKAGE}/lib/python3.12/site-packages/serena_agent-{}.dist-info",
            plan::SERENA_PIN
        );
        let script = format!(
            "#!/bin/sh\necho call >> '{log}'\nrm -rf \"$UV_TOOL_DIR/{PACKAGE}\"\nmkdir -p \"{site}\" \"$UV_TOOL_BIN_DIR\"\n\
             printf '{{\"vcs_info\":{{\"commit_id\":\"{commit}\"}}}}' > \"{site}/direct_url.json\"\n\
             : > \"$UV_TOOL_BIN_DIR/serena\"\nexit {exit}\n",
            log = t.join("calls").display(),
        );
        std::fs::write(&uv, script).unwrap();
        std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).unwrap();
        uv
    }

    fn calls(t: &Path) -> usize {
        std::fs::read_to_string(t.join("calls"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    #[test]
    fn installs_then_skips() {
        let t = tempfile::tempdir().unwrap();
        let uv = fake_uv(t.path(), SERENA_GIT_REV, 0);
        let dir = t.path().join("serena");
        let (o, r) = install_with(&uv, &dir).unwrap();
        assert_eq!(o, Outcome::Installed);
        assert_eq!(r.bin, serena_binary_path_in(&dir));
        let (o, _) = install_with(&uv, &dir).unwrap();
        assert_eq!(o, Outcome::AlreadyInstalled);
        assert_eq!(calls(t.path()), 1, "verified re-run must not call uv");
    }

    #[test]
    fn wrong_commit_after_install_is_an_error() {
        let t = tempfile::tempdir().unwrap();
        let uv = fake_uv(t.path(), &"f".repeat(40), 0);
        let e = install_with(&uv, &t.path().join("serena")).unwrap_err();
        assert!(e.to_string().contains(SERENA_GIT_REV), "{e:#}");
    }

    #[test]
    fn uv_failure_is_an_error() {
        let t = tempfile::tempdir().unwrap();
        let uv = fake_uv(t.path(), SERENA_GIT_REV, 3);
        let e = install_with(&uv, &t.path().join("serena")).unwrap_err();
        assert!(format!("{e:#}").contains("uv tool install"), "{e:#}");
    }

    #[test]
    fn mismatch_reinstalls_with_force() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("serena");
        fake_install(&dir, "1.7.0", SERENA_GIT_REV);
        let uv = fake_uv(t.path(), SERENA_GIT_REV, 0);
        let (o, _) = install_with(&uv, &dir).unwrap();
        assert_eq!(o, Outcome::Installed);
        assert_eq!(installed_state(&dir), Installed::Verified);
    }
}

#[test]
fn missing_uv_names_the_prerequisite() {
    let t = tempfile::tempdir().unwrap();
    let e = install_with(&t.path().join("no-such-uv"), &t.path().join("s")).unwrap_err();
    assert!(format!("{e:#}").contains("uv"), "{e:#}");
}
