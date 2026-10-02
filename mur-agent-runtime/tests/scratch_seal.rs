//! Per-agent scratch dir under a real seal (spec tests #5–#7).
//!
//! Each test re-execs this binary; a `#[ctor]` hook in the child applies
//! `sandbox::apply` for agent `<home>/agents/a` and then performs the probe.
//! The fake MUR home lives under `CARGO_TARGET_TMPDIR`, NOT the system temp
//! dir, because macOS's baseline write-exempts `/private/var/folders` and
//! `/private/tmp` — a home there would make the #6 deny vacuous.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

const CASE_ENV: &str = "MUR_TEST_SCRATCH_SEAL_CASE";
const HOME_ENV: &str = "MUR_TEST_SCRATCH_SEAL_HOME";
const REQUIRE_SANDBOX_ENV: &str = "MUR_TEST_REQUIRE_SANDBOX";
const EXIT_SANDBOX_REQUIRED: i32 = 4;
const AGENT: &str = "a";
const SIBLING: &str = "b";
const PROBE_FILE: &str = "x";
const SEALED_MARKER: &str = ".sealed";

fn fresh_home(case: &str) -> PathBuf {
    let home = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("scratch_seal")
        .join(format!("{case}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    for n in [AGENT, SIBLING] {
        std::fs::create_dir_all(home.join("agents").join(n)).unwrap();
        std::fs::create_dir_all(home.join("tmp").join(n)).unwrap();
    }
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

/// #5: `mktemp` and `$TMPDIR` writes land in the agent's own scratch dir.
#[test]
fn sealed_child_writes_its_own_scratch_dir() {
    let home = run_case("own");
    if was_sealed(&home) {
        assert!(home.join("tmp").join(AGENT).join(PROBE_FILE).is_file());
    }
}

/// #6: a sibling agent's scratch dir is denied.
#[test]
fn sealed_child_cannot_write_sibling_scratch_dir() {
    let home = run_case("sibling");
    assert!(!home.join("tmp").join(SIBLING).join(PROBE_FILE).exists());
}

/// #7: Linux only — `/tmp` is outside the seal. macOS allows it by design.
#[cfg(target_os = "linux")]
#[test]
fn sealed_child_cannot_write_system_tmp() {
    run_case("systmp");
}

/// The child drops `.sealed` in its scratch dir only after an enforcing seal
/// and a passing probe; absent means it took the skip path.
fn was_sealed(home: &Path) -> bool {
    home.join("tmp").join(AGENT).join(SEALED_MARKER).exists()
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
fn scratch_seal_subprocess_main() {
    let Some(case) = std::env::var_os(CASE_ENV) else {
        return;
    };
    let home = PathBuf::from(std::env::var_os(HOME_ENV).expect("home env"));
    let agent_home = home.join("agents").join(AGENT);
    let scratch = mur_agent_runtime::agent_paths::agent_scratch_dir(&agent_home)
        .unwrap_or_else(|e| fail(&format!("scratch dir: {e}")));

    let profile = mur_common::agent::AgentProfile::default_for_tests();
    match mur_agent_runtime::sandbox::apply(&profile.entitlements, &agent_home, &[], &[], &[]) {
        Ok(s) if s.enforcing => {}
        other => skip_or_fail_unenforced(&other),
    }

    match case.to_str().unwrap_or_default() {
        "own" => {
            let st = Command::new("sh")
                .arg("-c")
                .arg(format!(
                    r#"f=$(mktemp) && case "$f" in "$TMPDIR"/*) ;; *) exit 2;; esac && rm "$f" && touch "$TMPDIR/{PROBE_FILE}""#
                ))
                .envs(mur_agent_runtime::agent_paths::scratch_env(&scratch))
                .status()
                .unwrap_or_else(|e| fail(&format!("spawn sh: {e}")));
            if !st.success() {
                fail(&format!("mktemp/touch in scratch failed: {st}"));
            }
        }
        "sibling" => {
            let p = home.join("tmp").join(SIBLING).join(PROBE_FILE);
            if std::fs::write(&p, b"pwned").is_ok() {
                fail(&format!(
                    "write to sibling scratch {} was allowed",
                    p.display()
                ));
            }
        }
        "systmp" => {
            let p = Path::new("/tmp").join(format!("mur_scratch_seal_{}", std::process::id()));
            if std::fs::write(&p, b"pwned").is_ok() {
                let _ = std::fs::remove_file(&p);
                fail("write to /tmp was allowed");
            }
        }
        other => fail(&format!("unknown case {other}")),
    }
    let _ = std::fs::write(scratch.join(SEALED_MARKER), b"");
    std::process::exit(0);
}
