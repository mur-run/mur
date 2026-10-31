//! O2 env against a real `ssh -G` and a real bash.
//!
//! Skips with a printed reason when no `ssh` or `bash` is found, and fails
//! instead when `MUR_TEST_REQUIRE_SSH=1`, so a CI runner without ssh cannot
//! pass these without running them.

use super::ssh_client_env::{SSH_BIN, SshClientEnv, resolve_real_ssh};
use std::path::{Path, PathBuf};
use std::process::Command;

const REQUIRE_SSH_ENV: &str = "MUR_TEST_REQUIRE_SSH";
const HOST: &str = "example.com";

struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    env: SshClientEnv,
    bash: PathBuf,
}

fn exec_dirs() -> Vec<PathBuf> {
    crate::sandbox::search_dirs::spawn_search_dirs(&dirs_home())
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn fixture() -> Option<Fixture> {
    let dirs = exec_dirs();
    let found =
        resolve_real_ssh(&dirs).zip(dirs.iter().map(|d| d.join("bash")).find(|p| p.is_file()));
    let Some((_, bash)) = found else {
        let msg = format!("no {SSH_BIN} or bash in {dirs:?}");
        if std::env::var(REQUIRE_SSH_ENV).as_deref() == Ok("1") {
            panic!("{REQUIRE_SSH_ENV}=1 but {msg}");
        }
        eprintln!("skipping: {msg}");
        return None;
    };
    // A space and a `'` in the home exercise both quoting layers.
    let tmp = tempfile::Builder::new()
        .prefix("o2 it's")
        .tempdir()
        .unwrap();
    let home = tmp.path().to_path_buf();
    let env = SshClientEnv::resolve(&home, &dirs).expect("ssh resolved above");
    Some(Fixture {
        _tmp: tmp,
        home,
        env,
        bash,
    })
}

impl Fixture {
    fn run(&self, script: &str) -> String {
        let out = Command::new(&self.bash)
            .arg("-c")
            .arg(script)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .envs(self.env.env_pairs())
            .current_dir(&self.home)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{script}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn evil_conf(&self) -> PathBuf {
        let p = self.home.join("evil.conf");
        std::fs::write(
            &p,
            "StrictHostKeyChecking no\nUserKnownHostsFile /evil\nBatchMode no\n",
        )
        .unwrap();
        p
    }
}

fn value<'a>(g: &'a str, key: &str) -> &'a str {
    g.lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix(' '))
        .unwrap_or_else(|| panic!("no {key} in ssh -G output"))
}

fn assert_pinned(g: &str, home: &Path) {
    let kh = mur_common::agent::known_hosts_path(home);
    assert_eq!(value(g, "userknownhostsfile"), kh.to_string_lossy());
    assert_eq!(value(g, "globalknownhostsfile"), "none");
    assert_eq!(value(g, "stricthostkeychecking"), "true");
    assert_eq!(value(g, "identityfile"), "none");
    assert_eq!(value(g, "batchmode"), "yes");
}

fn q(p: &Path) -> String {
    shell_escape::unix::escape(p.to_string_lossy()).into_owned()
}

#[test]
fn bash_ssh_resolves_pinned_settings() {
    let Some(f) = fixture() else { return };
    assert_pinned(&f.run(&format!("ssh -G {HOST}")), &f.home);
}

#[test]
fn caller_flags_do_not_change_pinned_settings() {
    let Some(f) = fixture() else { return };
    let evil = q(&f.evil_conf());
    let g = f.run(&format!(
        "ssh -F {evil} -o UserKnownHostsFile=/evil -o StrictHostKeyChecking=no -G {HOST}"
    ));
    assert_pinned(&g, &f.home);
}

#[test]
fn ssh_is_a_function_in_bash() {
    let Some(f) = fixture() else { return };
    assert_eq!(f.run("type -t ssh").trim(), "function");
}

#[test]
fn function_returns_to_the_caller_shell() {
    let Some(f) = fixture() else { return };
    let out = f.run(&format!("ssh -G {HOST} >/dev/null; echo after"));
    assert_eq!(out.trim(), "after");
}

#[test]
fn git_ssh_command_resolves_pinned_settings_via_sh() {
    let Some(f) = fixture() else { return };
    let evil = q(&f.evil_conf());
    for extra in [
        String::new(),
        format!("-F {evil} -o StrictHostKeyChecking=no "),
    ] {
        let g = f.run(&format!("sh -c \"$GIT_SSH_COMMAND {extra}-G {HOST}\""));
        assert_pinned(&g, &f.home);
    }
}
