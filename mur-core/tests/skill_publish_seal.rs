//! First `mur skill publish` inside a sealed agent sandbox (startup-permission
//! -overrides spec, R6-5): with no `publisher/identity.key`, the publish fails
//! with one of the documented errors and creates no key. From a normal shell
//! the same command creates it.
//!
//! The sealed run happens in a re-exec of this test binary (the seal is
//! inherited by children and cannot be undone), which then spawns the `mur`
//! binary under it.
//!
//! `PATH` is an empty directory in both runs, so `mur skill publish` stops at
//! "`gh` not found" right after the key step and never forks the registry.
#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CHILD_ENV: &str = "MUR_TEST_PUBLISH_SEAL_ROOT";
const REQUIRE_SANDBOX_ENV: &str = "MUR_TEST_REQUIRE_SANDBOX";
const EXIT_SKIPPED: i32 = 3;
const EXIT_SANDBOX_REQUIRED: i32 = 4;
const AGENT: &str = "x";
const TEST_NAME: &str = "first_publish_inside_a_sandbox_creates_no_key";
/// The errors §"First `mur skill publish` inside a sandbox" lists.
const DOCUMENTED_ERRORS: [&str; 3] = [
    "Error: Operation not permitted (os error 1)",
    "Error: save publisher identity: io error:",
    "Error: load publisher identity: identity key exists but is not readable",
];
const GH_MISSING: &str = "GitHub CLI (`gh`) not found";

const SKILL: &str = "name: seal-probe
version: 1.0.0
publisher: human:t
description: probe
category: context
content:
  abstract: hi
  context: body
";

struct Layout {
    mur: PathBuf,
    home: PathBuf,
    empty_path: PathBuf,
    skill: PathBuf,
}

impl Layout {
    fn at(root: &Path) -> Self {
        Self {
            mur: root.join("mur"),
            home: root.join("home"),
            empty_path: root.join("empty-path"),
            skill: root.join("skill.yaml"),
        }
    }

    fn key(&self) -> PathBuf {
        self.mur.join("publisher").join("identity.key")
    }

    fn publish(&self) -> Output {
        Command::new(env!("CARGO_BIN_EXE_mur"))
            .args(["skill", "publish"])
            .arg(&self.skill)
            .env("MUR_HOME", &self.mur)
            .env("HOME", &self.home)
            .env("PATH", &self.empty_path)
            .env("MUR_KEYCHAIN_DISABLED", "1")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run mur skill publish")
    }
}

fn fresh_root() -> PathBuf {
    // CARGO_TARGET_TMPDIR, canonicalized (R7-4): not under the macOS baseline
    // write exemptions, and in the form Seatbelt matches against.
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join("publish_seal");
    std::fs::create_dir_all(&base).unwrap();
    let root = std::fs::canonicalize(&base)
        .unwrap()
        .join(std::process::id().to_string());
    let _ = std::fs::remove_dir_all(&root);
    let l = Layout::at(&root);
    for d in [&l.mur.join("agents").join(AGENT), &l.home, &l.empty_path] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(&l.skill, SKILL).unwrap();
    root
}

#[test]
fn first_publish_inside_a_sandbox_creates_no_key() {
    if let Some(root) = std::env::var_os(CHILD_ENV) {
        sealed_child(&Layout::at(Path::new(&root)));
    }
    let root = fresh_root();
    let l = Layout::at(&root);

    let sealed = Command::new(std::env::current_exe().unwrap())
        .args([TEST_NAME, "--exact", "--nocapture"])
        .env(CHILD_ENV, &root)
        .env("HOME", &l.home)
        .env_remove("MUR_AGENT_SKIP_SANDBOX")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&sealed.stderr);
    if sealed.status.code() == Some(EXIT_SKIPPED) {
        eprintln!("{stderr}");
        return;
    }
    assert_eq!(sealed.status.code(), Some(0), "sealed run:\n{stderr}");
    assert!(!l.key().exists(), "the sealed publish created a key");

    // From a normal shell the same command creates the key, then stops at gh.
    let out = l.publish();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        l.key().is_file(),
        "unsandboxed publish made no key:\n{stderr}"
    );
    assert!(stderr.contains(GH_MISSING), "did not stop at gh:\n{stderr}");
}

fn sealed_child(l: &Layout) -> ! {
    let mut profile = mur_common::agent::AgentProfile::default_for_tests();
    profile.entitlements.processes.spawn.allowed = vec![env!("CARGO_BIN_EXE_mur").to_string()];
    let agent_home = l.mur.join("agents").join(AGENT);
    match mur_agent_runtime::sandbox::apply(&profile.entitlements, &agent_home, &[], &[], &[]) {
        Ok(s) if s.enforcing => {}
        other => {
            if std::env::var_os(REQUIRE_SANDBOX_ENV).is_some_and(|v| v == "1") {
                eprintln!("FAIL: {REQUIRE_SANDBOX_ENV}=1 but sandbox not enforcing: {other:?}");
                std::process::exit(EXIT_SANDBOX_REQUIRED);
            }
            eprintln!("SKIP: sandbox not enforcing: {other:?}");
            std::process::exit(EXIT_SKIPPED);
        }
    }
    let out = l.publish();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let documented = DOCUMENTED_ERRORS.iter().any(|e| stderr.contains(e));
    if out.status.success() || !documented {
        eprintln!(
            "ERROR: want a documented failure, got {:?}:\n{stderr}",
            out.status
        );
        std::process::exit(1);
    }
    eprintln!("sealed publish failed as documented:\n{stderr}");
    std::process::exit(0);
}
