use super::*;
use mur_common::serena_ls_cmd::{INTELEPHENSE, INTELEPHENSE_PIN, TS_LS_PIN, TYPESCRIPT};
use std::ffi::OsStr;

fn fake_install(ls: &ManagedNodeLs, tools: &Path, version: &str) {
    let entry = ls.entry(tools);
    std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
    std::fs::write(&entry, b"#!/usr/bin/env node\n").unwrap();
    // The server at `version`; every other package at its pin.
    for (i, (name, pin)) in ls.packages.iter().enumerate() {
        let v = if i == 0 { version } else { pin };
        let pkg = ls.dir(tools).join("node_modules").join(name);
        std::fs::create_dir_all(&pkg).unwrap();
        let json = format!(r#"{{"name":"{name}","version":"{v}"}}"#);
        std::fs::write(pkg.join("package.json"), json).unwrap();
    }
}

fn args(cmd: &Command) -> Vec<String> {
    cmd.get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn command_pins_version_registry_and_skips_scripts() {
    let dir = Path::new("/h/tools/intelephense/1.14.4");
    let cmd = install_command(&INTELEPHENSE, Path::new("npm"), dir);
    let args = args(&cmd);
    assert_eq!(args[0], "install");
    assert!(args.iter().any(|a| a == "--ignore-scripts"), "{args:?}");
    let reg = args.iter().position(|a| a == "--registry").unwrap();
    assert_eq!(args[reg + 1], NPM_REGISTRY);
    let pre = args.iter().position(|a| a == "--prefix").unwrap();
    assert_eq!(Path::new(&args[pre + 1]), dir);
    assert_eq!(
        args.last().unwrap(),
        &format!("intelephense@{INTELEPHENSE_PIN}")
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

/// #1981 Layer 1: npm's cache lives in the install's own tools dir, so
/// setup never needs (and never asks for) write access to `~/.npm`.
#[test]
fn npm_cache_stays_inside_the_tools_dir() {
    let tools = Path::new("/h/.mur/tools");
    let dir = TYPESCRIPT.dir(tools);
    let args = args(&install_command(&TYPESCRIPT, Path::new("npm"), &dir));
    let c = args.iter().position(|a| a == "--cache").expect("--cache");
    assert_eq!(Path::new(&args[c + 1]), npm_cache_dir(tools));
    assert!(Path::new(&args[c + 1]).starts_with(tools));
    let ev = install_command(&TYPESCRIPT, Path::new("npm"), &dir);
    assert!(
        ev.get_envs()
            .any(|(e, v)| e == OsStr::new("NPM_CONFIG_CACHE") && v.is_none()),
        "NPM_CONFIG_CACHE not cleared"
    );
}

#[test]
fn typescript_installs_server_and_compiler_together() {
    let dir = Path::new("/t/typescript-language-server/5.1.3");
    let args = args(&install_command(&TYPESCRIPT, Path::new("npm"), dir));
    let n = args.len();
    assert_eq!(
        args[n - 2..],
        [
            format!("typescript-language-server@{TS_LS_PIN}"),
            "typescript@5.9.3".to_owned()
        ]
    );
}

#[test]
fn verified_only_for_the_pinned_version() {
    for ls in [&INTELEPHENSE, &TYPESCRIPT] {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(installed_state(ls, t.path()), Installed::Missing);
        fake_install(ls, t.path(), ls.pin());
        assert_eq!(
            installed_state(ls, t.path()),
            Installed::Verified,
            "{}",
            ls.ls_id
        );
    }
}

#[test]
fn other_version_is_a_mismatch() {
    let t = tempfile::tempdir().unwrap();
    fake_install(&TYPESCRIPT, t.path(), "1.0.0");
    assert_eq!(installed_state(&TYPESCRIPT, t.path()), Installed::Mismatch);
}

/// TypeScript's server without its pinned `typescript` is not verified.
#[test]
fn a_missing_companion_package_is_a_mismatch() {
    let t = tempfile::tempdir().unwrap();
    fake_install(&TYPESCRIPT, t.path(), TS_LS_PIN);
    let ts = TYPESCRIPT.dir(t.path()).join("node_modules/typescript");
    std::fs::remove_dir_all(ts).unwrap();
    assert_eq!(installed_state(&TYPESCRIPT, t.path()), Installed::Mismatch);
}

/// Installing one server must not satisfy another.
#[test]
fn servers_do_not_share_an_install() {
    let t = tempfile::tempdir().unwrap();
    fake_install(&INTELEPHENSE, t.path(), INTELEPHENSE_PIN);
    assert_eq!(installed_state(&TYPESCRIPT, t.path()), Installed::Missing);
}

#[test]
fn verified_install_skips_npm() {
    let t = tempfile::tempdir().unwrap();
    fake_install(&TYPESCRIPT, t.path(), TS_LS_PIN);
    // A nonexistent npm proves it was never run.
    let (o, r) = install_with(&TYPESCRIPT, Path::new("/nonexistent/npm"), t.path()).unwrap();
    assert_eq!(o, Outcome::AlreadyInstalled);
    assert_eq!(r.entry, TYPESCRIPT.entry(t.path()));
    assert_eq!(r.version, TS_LS_PIN);
}

/// The record's entry is the one the canonical `ls_base_cmd` names, so
/// setup and C7 agree by construction.
#[test]
fn record_entry_matches_the_canonical_base_cmd() {
    let tools = Path::new("/h/.mur/tools");
    for ls in [&INTELEPHENSE, &TYPESCRIPT] {
        let cmd = mur_common::serena_ls_cmd::canonical_base_cmd(ls.ls_id, tools).unwrap();
        assert_eq!(Path::new(&cmd[1]), Record::for_tools(ls, tools).entry);
    }
}

/// #1998 review: a Vue install whose packages verify but whose `.bin` shim
/// serena execs is gone is not complete, so setup reinstalls it.
#[test]
fn a_missing_required_shim_forces_a_reinstall() {
    use mur_common::serena_ls_cmd::{VUE, VUE_LS_PIN};
    let t = tempfile::tempdir().unwrap();
    fake_install(&VUE, t.path(), VUE_LS_PIN);
    for rel in VUE.required_rel {
        let mut p = VUE.dir(t.path());
        p.extend(*rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"").unwrap();
    }
    assert_eq!(installed_state(&VUE, t.path()), Installed::Verified);
    let mut shim = VUE.dir(t.path());
    shim.extend(VUE.required_rel[1]);
    std::fs::remove_file(&shim).unwrap();
    assert_eq!(installed_state(&VUE, t.path()), Installed::Missing);
    // Not short-circuited as AlreadyInstalled: npm is attempted.
    let e = install_with(&VUE, Path::new("/nonexistent/npm"), t.path()).unwrap_err();
    assert!(format!("{e:#}").contains("prerequisite"), "{e:#}");
}
