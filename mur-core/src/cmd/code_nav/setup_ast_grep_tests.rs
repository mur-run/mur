//! #2018: what a `mur code-nav setup` run that failed after wiring ast-grep
//! leaves behind. The installer is injected, so none of this needs the network
//! or the real binary and it runs in CI (except where noted).

use super::*;
use crate::cmd::code_nav::ast_grep_install::Outcome;
use crate::cmd::code_nav::{ast_grep_wire, consent, plan};
use std::io::BufRead;

fn fake_install(dest: &Path) -> Result<Outcome> {
    std::fs::create_dir_all(dest.parent().unwrap())?;
    std::fs::write(dest, b"not the real ast-grep")?;
    Ok(Outcome::Installed)
}

/// The plan `run` would build for `--with-serena` with ast-grep on and
/// `uv` on the host, with ast-grep's own grants added as `run` does.
fn plan_for(h: &Home) -> plan::Plan {
    let flags = Flags {
        with_serena: true,
        // One language with no prerequisites: the same plan on every host.
        lsp: vec!["lua".into()],
        ..Default::default()
    };
    let detected = Detected {
        on_path: [UV.to_owned()].into(),
    };
    let mut p = plan::plan(&h.mur_home, &flags, &detected).unwrap();
    let slot = ast_grep_wire::preflight("carol").unwrap();
    p.permissions
        .extend(ast_grep_wire::permissions(&h.mur_home, &slot));
    p
}

/// `apply` as `run` calls it, up to (not including) the manifest save.
fn apply_run(
    h: &Home,
    plan: &plan::Plan,
    consent: Consent,
    input: &mut dyn BufRead,
    install: &dyn Fn(&Path) -> Result<Outcome>,
) -> Result<Manifest> {
    let agent_home = h.mur_home.join("agents/carol");
    apply_with(
        &Target {
            mur_home: &h.mur_home,
            agent: "carol",
            agent_home: &agent_home,
            entry: "serena",
            manifest_path: &consent::manifest_path(&h.mur_home, "carol"),
        },
        plan,
        Some(&h.repo),
        Manifest::default(),
        &mut Ask { consent, input },
        &mut Vec::new(),
        install,
    )
}

/// A `uv` that always fails, alone on `PATH`: serena's install fails
/// after ast-grep was wired.
fn failing_uv(h: &mut Home) {
    use std::os::unix::fs::PermissionsExt;
    let stub = h.mur_home.parent().unwrap().join("path");
    std::fs::create_dir_all(&stub).unwrap();
    let uv = stub.join("uv");
    std::fs::write(&uv, b"#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).unwrap();
    h._env.set_var("PATH", &stub);
}

fn manifest_of(h: &Home) -> Manifest {
    consent::load_manifest(&consent::manifest_path(&h.mur_home, "carol")).unwrap()
}

/// The manifest on disk records every ast-grep grant the profile now
/// holds, and a re-run's diff does not list ast-grep as new.
fn assert_recorded(h: &Home, plan: &plan::Plan) {
    let m = manifest_of(h);
    let (_, profile) = crate::cmd::agent::load_profile_for_edit("carol").unwrap();
    let slot = ast_grep_wire::slot(&profile).unwrap();
    let bin = mur_common::config::ast_grep_binary_path(&h.mur_home);
    let run_dir = mur_common::config::ast_grep_runtime_dir(&h.mur_home);
    assert!(
        profile
            .entitlements
            .processes
            .spawn
            .allowed
            .contains(&bin.display().to_string()),
        "the profile has the spawn grant"
    );
    assert!(
        profile
            .entitlements
            .filesystem
            .write
            .contains(&run_dir.display().to_string()),
        "the profile has the run-dir grant"
    );
    for p in ast_grep_wire::permissions(&h.mur_home, &slot) {
        assert!(
            m.granted.contains(&consent::grant_key(&p)),
            "profile holds {p:?} but the manifest does not: {:?}",
            m.granted
        );
    }
    let d = consent::diff(plan, "serena", Some(&h.repo), &m);
    let ast_grep = |k: &String| k.contains("ast-grep") || k.contains("mur-tools");
    assert!(!d.installs.iter().any(ast_grep), "{:?}", d.installs);
    assert!(!d.grants.iter().any(ast_grep), "{:?}", d.grants);
}

#[cfg(unix)]
#[test]
fn a_failed_serena_install_keeps_the_ast_grep_grants_recorded() {
    let mut h = home_with_servers(&[]);
    failing_uv(&mut h);
    let plan = plan_for(&h);

    let err = apply_run(
        &h,
        &plan,
        Consent::Given,
        &mut std::io::empty(),
        &fake_install,
    )
    .expect_err("uv exits 1");
    assert!(format!("{err:#}").contains("uv tool install"), "{err:#}");

    let m = manifest_of(&h);
    assert!(
        m.installed.contains(&consent::install_key(
            plan::AST_GREP,
            mur_common::config::AST_GREP_PINNED_VERSION
        )),
        "{:?}",
        m.installed
    );
    assert_recorded(&h, &plan);
}

/// Fails before ast-grep is wired: nothing was granted, so nothing is
/// recorded and no manifest appears.
#[test]
fn a_failure_before_the_wiring_records_nothing() {
    let h = home_with_servers(&[]);
    let plan = plan_for(&h);
    let before = snapshot(&h.mur_home);

    let boom = |_: &Path| -> Result<Outcome> { bail!("download failed") };
    let err = apply_run(&h, &plan, Consent::Given, &mut std::io::empty(), &boom)
        .expect_err("install fails");
    assert!(format!("{err:#}").contains("download failed"));
    assert_eq!(snapshot(&h.mur_home), before, "something was written");
}

/// The user declines the exec lanes: serena is installed and its files are
/// written, then the run stops before the profile entry. The ast-grep grants
/// were already saved to the profile, so the manifest must hold them.
///
/// Needs serena's pinned templates (GPL, not vendored): copied from a real
/// install, like the success tests.
#[cfg(unix)]
#[test]
#[ignore = "needs a pinned serena install for its templates (MUR_SERENA_INSTALL_DIR)"]
fn declined_lanes_keep_the_ast_grep_grants_recorded() {
    let real =
        PathBuf::from(std::env::var_os("MUR_SERENA_INSTALL_DIR").expect("MUR_SERENA_INSTALL_DIR"));
    let mut h = home_with_servers(&[]);
    super::success::install_fake_serena(&mut h, &real);
    let plan = plan_for(&h);

    let err = apply_run(&h, &plan, Consent::Ask, &mut &b"no\n"[..], &fake_install)
        .expect_err("lanes declined");
    assert!(
        format!("{err:#}").contains("exec lanes not approved"),
        "{err:#}"
    );

    assert_recorded(&h, &plan);
    let (_, profile) = crate::cmd::agent::load_profile_for_edit("carol").unwrap();
    assert!(
        !profile.mcp_servers.iter().any(|e| e.name == "serena"),
        "the declined run left no serena entry"
    );
}
