use super::*;
use crate::cmd::code_nav::plan::{self, Detected, Flags};
use std::collections::BTreeSet;
use std::ffi::OsStr;

/// Lay down what `uv tool install pyright` leaves behind.
fn fake_install(dir: &Path, version: &str) {
    let site = dir
        .join(UV_TOOL_SUBDIR)
        .join(PACKAGE)
        .join("lib/python3.13/site-packages")
        .join(format!("{PACKAGE}-{version}.dist-info"));
    std::fs::create_dir_all(&site).unwrap();
    let bin = langserver_path_in(dir);
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
}

fn env_of(cmd: &Command, key: &str) -> Option<PathBuf> {
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
            lsp: vec!["python".into()],
            ..Default::default()
        },
        &Detected {
            on_path: BTreeSet::from(["uv".to_string(), "node".to_string()]),
        },
    )
    .unwrap();
    let row = p.install.iter().find(|r| r.name == PYRIGHT).unwrap();
    assert_eq!(pyright_dir(home), row.dir);
    assert_eq!(row.version, PYRIGHT_PIN);
}

#[test]
fn command_pins_version_and_cutoff() {
    let cmd = install_command(Path::new("uv"), Path::new("/d"), false);
    let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy()).collect();
    assert_eq!(&args[..2], ["tool", "install"]);
    assert!(args.iter().any(|a| a == "--no-config"), "{args:?}");
    let cut = args.iter().position(|a| a == "--exclude-newer").unwrap();
    assert_eq!(args[cut + 1], PYRIGHT_EXCLUDE_NEWER);
    assert_eq!(args.last().unwrap(), &format!("pyright=={PYRIGHT_PIN}"));
    assert!(!args.iter().any(|a| a == "--reinstall"));
}

#[test]
fn command_confines_uv_to_the_managed_dir() {
    let dir = Path::new("/h/tools/pyright/1.1.403");
    let cmd = install_command(Path::new("uv"), dir, false);
    assert_eq!(env_of(&cmd, "UV_TOOL_DIR"), Some(dir.join(UV_TOOL_SUBDIR)));
    assert_eq!(env_of(&cmd, "UV_TOOL_BIN_DIR"), Some(dir.join(BIN_SUBDIR)));
    assert_eq!(
        env_of(&cmd, "UV_PYTHON_INSTALL_DIR"),
        Some(dir.join(PYTHON_SUBDIR))
    );
    assert_eq!(cmd.get_current_dir(), Some(dir), "no repo pyproject in cwd");
}

#[test]
fn reinstall_flag_only_when_asked() {
    let cmd = install_command(Path::new("uv"), Path::new("/d"), true);
    assert!(cmd.get_args().any(|a| a == "--reinstall"));
}

#[test]
fn verified_only_for_the_pinned_version() {
    let t = tempfile::tempdir().unwrap();
    assert_eq!(installed_state(t.path()), Installed::Missing);
    fake_install(t.path(), PYRIGHT_PIN);
    assert_eq!(installed_state(t.path()), Installed::Verified);
}

#[test]
fn other_version_is_a_mismatch() {
    let t = tempfile::tempdir().unwrap();
    fake_install(t.path(), "1.1.999");
    assert_eq!(installed_state(t.path()), Installed::Mismatch);
}

#[test]
fn missing_entry_point_is_not_verified() {
    let t = tempfile::tempdir().unwrap();
    fake_install(t.path(), PYRIGHT_PIN);
    std::fs::remove_file(langserver_path_in(t.path())).unwrap();
    assert_eq!(installed_state(t.path()), Installed::Missing);
}

#[test]
fn verified_rerun_skips_uv() {
    let t = tempfile::tempdir().unwrap();
    fake_install(t.path(), PYRIGHT_PIN);
    // A uv that does not exist proves it was never called.
    let (o, r) = install_with(&t.path().join("no-such-uv"), t.path()).unwrap();
    assert_eq!(o, Outcome::AlreadyInstalled);
    assert_eq!(r.bin, langserver_path_in(t.path()));
}

#[test]
fn missing_uv_names_the_prerequisite() {
    let t = tempfile::tempdir().unwrap();
    let e = install_with(&t.path().join("no-such-uv"), &t.path().join("p")).unwrap_err();
    assert!(format!("{e:#}").contains("uv"), "{e:#}");
}

#[test]
fn record_carries_the_pin_and_ls_path() {
    let r = Record::for_dir(Path::new("/d"));
    assert_eq!(r.version, PYRIGHT_PIN);
    assert_eq!(r.exclude_newer, PYRIGHT_EXCLUDE_NEWER);
    assert!(
        r.bin
            .ends_with(format!("bin/{ENTRY_POINT}{}", std::env::consts::EXE_SUFFIX))
    );
}
