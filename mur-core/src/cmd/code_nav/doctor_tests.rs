//! #1983: `mur code-nav doctor` against a clean setup laid out in a tempdir,
//! then one kind of drift at a time.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::*;
use crate::cmd::code_nav::serena_install;

const AGENT: &str = "carol";

struct Fixture {
    _tmp: tempfile::TempDir,
    mur_home: PathBuf,
    agent_home: PathBuf,
    user_home: PathBuf,
    repo: PathBuf,
    manifest: Manifest,
    profile: AgentProfile,
    ast_grep_pin: Option<ast_grep_install::Pinned>,
}

const AST_GREP_BIN: &[u8] = b"#!/bin/sh\necho ast-grep\n";

fn sha(b: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(b))
}

/// What `uv tool install` leaves for serena at `version`: the entry point,
/// the dist-info with the pinned commit, a uv-managed interpreter.
fn fake_serena(dir: &Path, version: &str) {
    use serena_install::{PACKAGE, PYTHON_SUBDIR, PYVENV_CFG, SERENA_GIT_REV, SERENA_GIT_URL};
    let env = dir.join(serena_install::UV_TOOL_SUBDIR).join(PACKAGE);
    let site = env
        .join("lib/python3.12/site-packages")
        .join(format!("serena_agent-{version}.dist-info"));
    std::fs::create_dir_all(&site).unwrap();
    std::fs::write(
        site.join("direct_url.json"),
        format!(
            r#"{{"url":"{SERENA_GIT_URL}","vcs_info":{{"vcs":"git","commit_id":"{SERENA_GIT_REV}"}}}}"#
        ),
    )
    .unwrap();
    let home = dir.join(PYTHON_SUBDIR).join("cpython/bin");
    std::fs::write(env.join(PYVENV_CFG), format!("home = {}\n", home.display())).unwrap();
    touch(&serena_install::serena_binary_path_in(dir));
}

fn touch(p: &Path) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, b"").unwrap();
}

/// A clean setup: ast-grep and serena installed, one read and one spawn
/// grant recorded and present, the default entry's `project.yml` listing
/// exactly the recorded languages.
fn clean() -> Fixture {
    // The tempdir name deliberately carries a language name: `tempfile`'s
    // random suffix once spelled `/tmp/.tmphpSj7S` in CI and tripped an
    // assertion that grepped the whole command line for "php". Any check on
    // a fix string must look at its flags, never at the path.
    let tmp = tempfile::Builder::new().prefix(".tmphp").tempdir().unwrap();
    let mur_home = tmp.path().join("mur-home");
    let agent_home = mur_home.join("agents").join(AGENT);
    let user_home = tmp.path().join("user");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&user_home).unwrap();

    let ag = mur_common::config::ast_grep_binary_path(&mur_home);
    touch(&ag);
    std::fs::write(&ag, AST_GREP_BIN).unwrap();
    let ast_grep_pin = Some(ast_grep_install::Pinned {
        target: "test-target",
        zip_sha256: String::new(),
        member: "ast-grep",
        bin_sha256: sha(AST_GREP_BIN),
    });
    let serena_dir = serena_install::serena_dir(&mur_home);
    let serena = serena_install::Record::for_dir(&serena_dir);
    fake_serena(&serena_dir, plan::SERENA_PIN);

    let langs: BTreeSet<String> = ["php".to_string(), "python".to_string()].into();
    let pfile = agent_home
        .join(SERENA_HOME_DIR)
        .join("projects")
        .join(repo.file_name().unwrap())
        .join(PROJECT_FILE);
    std::fs::create_dir_all(pfile.parent().unwrap()).unwrap();
    std::fs::write(
        &pfile,
        "project_name: repo\nlanguage_servers: [\"php\", \"python\"]\n",
    )
    .unwrap();

    let read = serena_dir.display().to_string();
    let mut profile = AgentProfile::default_for_tests();
    profile.name = AGENT.into();
    profile.entitlements.filesystem.read.push(read.clone());
    profile
        .entitlements
        .processes
        .spawn
        .allowed
        .push("node".into());

    let manifest = Manifest {
        installed: [
            consent::install_key(plan::AST_GREP, mur_common::config::AST_GREP_PINNED_VERSION),
            consent::install_key(plan::SERENA, plan::SERENA_PIN),
        ]
        .into(),
        granted: [format!("read {read}"), "spawn node".to_string()].into(),
        languages: langs.clone(),
        project: Some(repo.clone()),
        entry_projects: BTreeMap::new(),
        entry_languages: [(DEFAULT_ENTRY_NAME.to_string(), langs)].into(),
        serena: Some(serena),
        ..Default::default()
    };
    Fixture {
        _tmp: tmp,
        mur_home,
        agent_home,
        user_home,
        repo,
        manifest,
        profile,
        ast_grep_pin,
    }
}

fn run_audit(f: &Fixture) -> Vec<Finding> {
    audit(&Inputs {
        agent: AGENT,
        mur_home: &f.mur_home,
        agent_home: &f.agent_home,
        user_home: &f.user_home,
        manifest: &f.manifest,
        profile: &f.profile,
        ast_grep_pin: f.ast_grep_pin.clone(),
    })
}

fn warns(fs: &[Finding]) -> Vec<&Finding> {
    fs.iter().filter(|f| f.level != Level::Ok).collect()
}

#[test]
fn a_clean_setup_is_all_ok() {
    let f = clean();
    let fs = run_audit(&f);
    assert!(warns(&fs).is_empty(), "{fs:#?}");
    // 2 installs + 2 grants + 1 entry.
    assert_eq!(fs.len(), 5, "{fs:#?}");
}

#[test]
fn a_deleted_install_names_the_path_and_the_setup_command() {
    let f = clean();
    let bin = f.manifest.serena.as_ref().unwrap().bin.clone();
    std::fs::remove_file(&bin).unwrap();
    let fs = run_audit(&f);
    let w = warns(&fs);
    assert_eq!(w.len(), 1, "{fs:#?}");
    assert!(w[0].subject.starts_with("install serena@"));
    assert!(w[0].detail.contains(&bin.display().to_string()));
    let fix = w[0].fix.as_deref().unwrap();
    assert!(fix.starts_with(&format!("mur code-nav setup --agent {AGENT} --with-serena")));
    assert!(fix.contains(&f.repo.display().to_string()));
}

#[test]
fn an_install_upgraded_in_place_is_flagged_though_its_path_exists() {
    let f = clean();
    let dir = serena_install::serena_dir(&f.mur_home);
    // Same entry point, but the env now holds another serena version.
    std::fs::remove_dir_all(dir.join(serena_install::UV_TOOL_SUBDIR)).unwrap();
    fake_serena(&dir, "0.0.1");
    let fs = run_audit(&f);
    let w = warns(&fs);
    assert_eq!(w.len(), 1, "{fs:#?}");
    assert!(w[0].subject.starts_with("install serena@"));
    assert!(w[0].detail.contains("is not serena@"), "{}", w[0].detail);
    assert!(
        w[0].fix
            .as_deref()
            .unwrap()
            .starts_with("mur code-nav setup")
    );
}

#[test]
fn a_replaced_ast_grep_binary_is_flagged_by_its_hash() {
    let f = clean();
    std::fs::write(
        mur_common::config::ast_grep_binary_path(&f.mur_home),
        b"other",
    )
    .unwrap();
    let w = run_audit(&f)
        .into_iter()
        .filter(|x| x.level != Level::Ok)
        .collect::<Vec<_>>();
    assert_eq!(w.len(), 1, "{w:#?}");
    assert!(w[0].subject.starts_with("install ast-grep@"));
}

#[test]
fn no_offline_pin_says_the_version_is_unverified_not_healthy() {
    let mut f = clean();
    f.ast_grep_pin = None;
    let fs = run_audit(&f);
    let ag = fs
        .iter()
        .find(|x| x.subject.starts_with("install ast-grep@"))
        .unwrap();
    assert_eq!(ag.level, Level::Ok);
    assert!(
        ag.detail.contains("not verifiable offline"),
        "{}",
        ag.detail
    );
}

#[test]
fn a_recorded_version_this_mur_no_longer_pins_is_flagged() {
    let mut f = clean();
    f.manifest.installed = [consent::install_key(plan::AST_GREP, "0.0.1")].into();
    let w = run_audit(&f)
        .into_iter()
        .filter(|x| x.level != Level::Ok)
        .collect::<Vec<_>>();
    assert_eq!(w.len(), 1);
    assert!(
        w[0].detail
            .contains(mur_common::config::AST_GREP_PINNED_VERSION)
    );
}

#[test]
fn a_removed_grant_names_the_exact_perm_command() {
    let mut f = clean();
    f.profile.entitlements.processes.spawn.allowed.clear();
    let fs = run_audit(&f);
    let w = warns(&fs);
    assert_eq!(w.len(), 1, "{fs:#?}");
    assert_eq!(w[0].subject, "grant spawn node");
    assert_eq!(
        w[0].fix.as_deref(),
        Some(format!("mur agent perm allow-spawn {AGENT} node").as_str())
    );
}

#[test]
fn a_manual_npm_cache_write_grant_is_flagged_with_its_revoke() {
    let mut f = clean();
    f.profile
        .entitlements
        .filesystem
        .write
        .push("~/.npm".into());
    let fs = run_audit(&f);
    let w = warns(&fs);
    assert_eq!(w.len(), 1, "{fs:#?}");
    assert_eq!(w[0].subject, "write ~/.npm");
    assert!(w[0].detail.contains("shared npm cache"));
    assert_eq!(
        w[0].fix.as_deref(),
        Some(format!("mur agent perm remove-path {AGENT} write ~/.npm").as_str())
    );
}

#[test]
fn an_unrecorded_spawn_dir_into_a_serena_home_is_flagged() {
    let mut f = clean();
    let lane = f
        .agent_home
        .join(SERENA_ENTRY_HOMES_DIR)
        .join("x")
        .display()
        .to_string();
    f.profile
        .entitlements
        .processes
        .spawn
        .allowed_dirs
        .push(lane.clone());
    let fs = run_audit(&f);
    let w = warns(&fs);
    assert_eq!(w.len(), 1, "{fs:#?}");
    assert_eq!(
        w[0].fix.as_deref(),
        Some(format!("mur agent perm deny-spawn-dir {AGENT} {lane}").as_str())
    );
}

#[test]
fn project_yml_languages_drifting_from_the_manifest_are_flagged() {
    let f = clean();
    let pfile = f
        .agent_home
        .join(SERENA_HOME_DIR)
        .join("projects")
        .join(f.repo.file_name().unwrap())
        .join(PROJECT_FILE);
    std::fs::write(&pfile, "language_servers: [\"python\"]\n").unwrap();
    let fs = run_audit(&f);
    let w = warns(&fs);
    assert_eq!(w.len(), 1, "{fs:#?}");
    assert!(w[0].detail.contains("[python]") && w[0].detail.contains("[php,python]"));
    // A language dropped from project.yml is usually one whose server cannot
    // start, so the fix must not re-add it (#1989): it narrows the recorded
    // set to what project.yml lists, via --lsp-only (#1992).
    assert!(w[0].detail.contains("missing [php]"), "{}", w[0].detail);
    let fix = w[0].fix.as_deref().expect("a narrowing fix");
    // Judge the flags, not the whole line: the project path sits in a
    // tempdir whose random name can itself contain "php".
    let (_, flags) = fix.split_once(" --entry ").expect("an --entry flag");
    assert_eq!(flags, "serena --lsp-only python", "{fix}");
}

#[test]
fn extra_project_yml_languages_get_the_rewrite_fix() {
    let f = clean();
    let pfile = f
        .agent_home
        .join(SERENA_HOME_DIR)
        .join("projects")
        .join(f.repo.file_name().unwrap())
        .join(PROJECT_FILE);
    std::fs::write(
        &pfile,
        "language_servers: [\"php\", \"python\", \"rust\"]\n",
    )
    .unwrap();
    let fs = run_audit(&f);
    let w = warns(&fs);
    assert_eq!(w.len(), 1, "{fs:#?}");
    let fix = w[0].fix.as_deref().unwrap();
    assert!(
        fix.ends_with("--entry serena --lsp php --lsp python"),
        "{fix}"
    );
}

/// Writes the fixture under an isolated `MUR_HOME` so `doctor_agent` reads
/// it the way the command does.
fn persist(f: &Fixture) {
    std::fs::write(
        f.agent_home.join("profile.yaml"),
        serde_yaml_ng::to_string(&f.profile).unwrap(),
    )
    .unwrap();
    consent::save_manifest(&consent::manifest_path(&f.mur_home, AGENT), &f.manifest).unwrap();
}

fn doctor(f: &Fixture) -> (usize, String) {
    let mut out = Vec::new();
    let n = doctor_agent(
        &f.mur_home,
        &f.user_home,
        AGENT,
        f.ast_grep_pin.as_ref(),
        &mut out,
    )
    .unwrap();
    (n, String::from_utf8(out).unwrap())
}

#[test]
fn a_corrupt_manifest_is_an_error_not_nothing_set_up() {
    let f = clean();
    persist(&f);
    std::fs::write(consent::manifest_path(&f.mur_home, AGENT), b"{ not json").unwrap();
    let (n, out) = doctor(&f);
    assert_eq!(n, 1);
    assert!(out.contains("error manifest"), "{out}");
    assert!(!out.contains("has not run"), "{out}");
}

#[test]
fn an_absent_manifest_dir_lists_no_agents() {
    let f = clean();
    assert!(manifest_agents(&f.mur_home).unwrap().is_empty());
}

#[test]
fn an_unreadable_manifest_dir_is_an_error_not_nothing_set_up() {
    let f = clean();
    let mut dir = f.mur_home.clone();
    dir.extend(consent::MANIFEST_DIR);
    // A file where the directory should be: read_dir fails, but not NotFound.
    touch(&dir);
    let e = manifest_agents(&f.mur_home).unwrap_err();
    assert!(
        format!("{e:#}").contains(&dir.display().to_string()),
        "{e:#}"
    );
}

#[test]
fn a_missing_manifest_says_setup_has_not_run() {
    let f = clean();
    let (n, out) = doctor(&f);
    assert_eq!(n, 0);
    assert!(out.contains("has not run"), "{out}");
}

fn bytes(p: &Path) -> Vec<u8> {
    std::fs::read(p).unwrap()
}

#[test]
fn doctor_writes_nothing() {
    let mut f = clean();
    f.profile.entitlements.processes.spawn.allowed.clear();
    persist(&f);
    let mpath = consent::manifest_path(&f.mur_home, AGENT);
    let ppath = f.agent_home.join("profile.yaml");
    let (m0, p0) = (bytes(&mpath), bytes(&ppath));
    let (n, out) = doctor(&f);
    assert_eq!(n, 1, "{out}");
    assert_eq!(bytes(&mpath), m0);
    assert_eq!(bytes(&ppath), p0);
}

/// #1982: doctor re-checks each entry's Vue link like the runtime does.
#[cfg(unix)]
#[test]
fn a_vue_entry_without_its_link_is_flagged_and_an_intact_one_is_ok() {
    use mur_common::serena_vue::{MARKER_FILE, marker, resources_link, ts_shim, vue_shim};
    let mut f = clean();
    f.manifest
        .entry_languages
        .get_mut(DEFAULT_ENTRY_NAME)
        .unwrap()
        .insert("vue".into());
    let w = warns(&run_audit(&f))
        .into_iter()
        .find(|x| x.subject.contains("vue link"))
        .expect("missing link flagged")
        .clone();
    assert!(w.detail.contains("npm-install"), "{w:?}");
    assert!(w.fix.unwrap().contains("--with-serena"));

    let tools = node_ls_install_tools(&f.mur_home);
    let dir = mur_common::serena_ls_cmd::VUE.dir(&tools);
    for shim in [vue_shim(&dir), ts_shim(&dir)] {
        touch(&shim);
    }
    std::fs::write(dir.join(MARKER_FILE), marker()).unwrap();
    let link = resources_link(&f.agent_home.join(SERENA_HOME_DIR));
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    let all = run_audit(&f);
    let ok = all.iter().find(|x| x.subject.contains("vue link")).unwrap();
    assert_eq!(ok.level, Level::Ok, "{ok:?}");
}
