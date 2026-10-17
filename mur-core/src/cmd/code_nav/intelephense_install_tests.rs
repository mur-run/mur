use super::*;
use std::ffi::OsStr;

fn fake_install(tools: &Path, version: &str) {
    let entry = intelephense_entry(tools);
    std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
    std::fs::write(&entry, b"#!/usr/bin/env node\n").unwrap();
    let pkg = intelephense_dir(tools).join("node_modules/intelephense/package.json");
    std::fs::write(
        pkg,
        format!(r#"{{"name":"intelephense","version":"{version}"}}"#),
    )
    .unwrap();
}

#[test]
fn command_pins_version_registry_and_skips_scripts() {
    let dir = Path::new("/h/tools/intelephense/1.14.4");
    let cmd = install_command(Path::new("npm"), dir);
    let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy()).collect();
    assert_eq!(args[0], "install");
    assert!(args.iter().any(|a| a == "--ignore-scripts"), "{args:?}");
    let reg = args.iter().position(|a| a == "--registry").unwrap();
    assert_eq!(args[reg + 1], NPM_REGISTRY);
    let pre = args.iter().position(|a| a == "--prefix").unwrap();
    assert_eq!(Path::new(args[pre + 1].as_ref()), dir);
    assert_eq!(
        args.last().unwrap(),
        &format!("{INTELEPHENSE_PACKAGE}@{INTELEPHENSE_PIN}")
    );
    assert_eq!(
        cmd.get_current_dir(),
        Some(dir),
        "no repo package.json in cwd"
    );
    for k in NPM_STEERING_ENV {
        assert!(
            cmd.get_envs()
                .any(|(e, v)| e == OsStr::new(k) && v.is_none()),
            "{k} not cleared"
        );
    }
}

#[test]
fn verified_only_for_the_pinned_version() {
    let t = tempfile::tempdir().unwrap();
    assert_eq!(installed_state(t.path()), Installed::Missing);
    fake_install(t.path(), INTELEPHENSE_PIN);
    assert_eq!(installed_state(t.path()), Installed::Verified);
}

#[test]
fn other_version_is_a_mismatch() {
    let t = tempfile::tempdir().unwrap();
    fake_install(t.path(), "1.0.0");
    assert_eq!(installed_state(t.path()), Installed::Mismatch);
}

#[test]
fn verified_install_skips_npm() {
    let t = tempfile::tempdir().unwrap();
    fake_install(t.path(), INTELEPHENSE_PIN);
    // A nonexistent npm proves it was never run.
    let (o, r) = install_with(Path::new("/nonexistent/npm"), t.path()).unwrap();
    assert_eq!(o, Outcome::AlreadyInstalled);
    assert_eq!(r.entry, intelephense_entry(t.path()));
}

/// The record's entry is the one the canonical `ls_base_cmd` names, so
/// setup and C7 agree by construction.
#[test]
fn record_entry_matches_the_canonical_base_cmd() {
    let tools = Path::new("/h/.mur/tools");
    let cmd =
        mur_common::serena_ls_cmd::canonical_base_cmd(mur_common::serena_ls_cmd::PHP_LS_ID, tools)
            .unwrap();
    assert_eq!(Path::new(&cmd[1]), Record::for_tools(tools).entry);
}
