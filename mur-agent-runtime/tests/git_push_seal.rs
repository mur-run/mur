//! Git-push registry decision, premise 1, under a REAL seal: the agent can read
//! `<mur_home>/git-push/registry.yaml` but cannot write it — even when its
//! profile grants write over the whole `<mur_home>` (on Linux that grant is
//! dropped whole; on macOS SBPL denies the broker dir after the allow).
//!
//! Re-exec pattern from `scratch_seal.rs`: a `#[ctor]` hook in the child seals
//! for agent `<home>/agents/a` and runs the probe. The fake home lives under
//! `CARGO_TARGET_TMPDIR`, not the system temp dir, whose macOS baseline is
//! write-exempt and would make the deny vacuous.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

const CASE_ENV: &str = "MUR_TEST_GIT_PUSH_SEAL_CASE";
const HOME_ENV: &str = "MUR_TEST_GIT_PUSH_SEAL_HOME";
const REQUIRE_SANDBOX_ENV: &str = "MUR_TEST_REQUIRE_SANDBOX";
const EXIT_SANDBOX_REQUIRED: i32 = 4;
const AGENT: &str = "a";
const ORIGINAL: &str = "repos: {}\n";
const SEALED_MARKER: &str = ".sealed";

fn fresh_home(case: &str) -> PathBuf {
    let home = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("git_push_seal")
        .join(format!("{case}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("agents").join(AGENT)).unwrap();
    std::fs::create_dir_all(mur_common::git_push::broker_dir(&home)).unwrap();
    std::fs::write(mur_common::git_push::registry_path(&home), ORIGINAL).unwrap();
    // Enabled and allowlisted, as in a real enrollment: on Landlock the registry read grant is
    // config-gated, and without it the "stays readable" half fails for the
    // wrong reason.
    std::fs::write(
        home.join("config.yaml"),
        format!("git_push:\n  enabled: true\n  agents: [{AGENT}]\n"),
    )
    .unwrap();
    home
}

fn run_case(case: &str) -> PathBuf {
    let home = fresh_home(case);
    let out = Command::new(std::env::current_exe().unwrap())
        .env(CASE_ENV, case)
        .env(HOME_ENV, &home)
        .env_remove("MUR_AGENT_SKIP_SANDBOX")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "case {case} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    home
}

fn was_sealed(home: &Path) -> bool {
    home.join("agents").join(AGENT).join(SEALED_MARKER).exists()
}

/// The registry survives a sealed agent that holds a write grant over all of `<mur_home>`.
#[test]
fn sealed_agent_cannot_rewrite_the_registry_even_under_a_mur_home_grant() {
    let home = run_case("registry");
    assert_eq!(
        std::fs::read_to_string(mur_common::git_push::registry_path(&home)).unwrap(),
        ORIGINAL
    );
    assert!(
        !mur_common::git_push::broker_dir(&home)
            .join("planted")
            .exists()
    );
}

/// Negative control: the same seal still lets the agent drop requests in its own inbox.
#[test]
fn sealed_agent_can_still_write_its_own_inbox() {
    let home = run_case("inbox");
    if was_sealed(&home) {
        let inbox = mur_common::git_push::inbox_dir(&home.join("agents").join(AGENT));
        assert!(inbox.join("r1.yaml").is_file());
    }
}

fn skip_or_fail_unenforced(why: &dyn std::fmt::Debug) -> ! {
    if std::env::var_os(REQUIRE_SANDBOX_ENV).is_some_and(|v| v == "1") {
        eprintln!("FAIL: {REQUIRE_SANDBOX_ENV}=1 but sandbox not enforcing: {why:?}");
        std::process::exit(EXIT_SANDBOX_REQUIRED);
    }
    eprintln!("SKIP: sandbox not enforcing: {why:?}");
    std::process::exit(0);
}

fn fail(msg: &str) -> ! {
    eprintln!("ERROR: {msg}");
    std::process::exit(1);
}

#[ctor::ctor]
fn git_push_seal_subprocess_main() {
    let Some(case) = std::env::var_os(CASE_ENV) else {
        return;
    };
    let home = PathBuf::from(std::env::var_os(HOME_ENV).expect("home env"));
    let agent_home = home.join("agents").join(AGENT);

    let mut profile = mur_common::agent::AgentProfile::default_for_tests();
    // The widest grant a user could plausibly write.
    profile
        .entitlements
        .filesystem
        .write
        .push(home.to_string_lossy().into_owned());
    match mur_agent_runtime::sandbox::apply(&profile.entitlements, &agent_home, &[], &[], &[]) {
        Ok(s) if s.enforcing => {}
        other => skip_or_fail_unenforced(&other),
    }

    match case.to_str().unwrap_or_default() {
        "registry" => {
            let reg = mur_common::git_push::registry_path(&home);
            if std::fs::read_to_string(&reg).is_err() {
                fail("registry must stay readable under the seal");
            }
            if std::fs::write(&reg, b"repos: {x: {path: /}}\n").is_ok() {
                fail("write to the git-push registry was allowed");
            }
            let planted = mur_common::git_push::broker_dir(&home).join("planted");
            if std::fs::write(&planted, b"x").is_ok() {
                fail("create inside the git-push broker dir was allowed");
            }
        }
        "inbox" => {
            let inbox = mur_common::git_push::inbox_dir(&agent_home);
            if let Err(e) = std::fs::create_dir_all(&inbox)
                .and_then(|_| std::fs::write(inbox.join("r1.yaml"), b"x"))
            {
                fail(&format!("own inbox write refused: {e}"));
            }
        }
        other => fail(&format!("unknown case {other}")),
    }
    let _ = std::fs::write(agent_home.join(SEALED_MARKER), b"");
    std::process::exit(0);
}
