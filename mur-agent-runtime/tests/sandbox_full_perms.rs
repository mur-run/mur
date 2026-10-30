//! `--full-perms` under a REAL kernel seal (macOS SBPL / Linux Landlock).
//!
//! The override resets `filesystem.deny` to `DEFAULT_DENY_PATHS`
//! (`startup_override::merge::apply`). On Linux (#2160) a user grant that
//! overlaps a user deny is dropped whole, so the reset is what brings the
//! grant back; on macOS the reset removes the trailing `deny` clause. Either
//! way the observable is the same, and so is the boundary: `~/.ssh` stays
//! unwritable because it is a default deny, never a user one.
//!
//! Shape: the test re-execs its own binary; a `#[ctor]` in the child builds
//! the profile, optionally merges the override, seals, and probes writes.
//! `HOME` points into the tempdir so `~/.ssh` never touches the real one.
//! Exit codes name the failure.

#[cfg(unix)]
mod unix {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    const ROOT_ENV: &str = "MUR_TEST_FULL_PERMS_ROOT";
    const MODE_ENV: &str = "MUR_TEST_FULL_PERMS_MODE";
    const MODE_OVERRIDE: &str = "override";
    const MODE_NORMAL: &str = "normal";
    /// Same contract as `sandbox_e2e.rs`: CI sets it so a runner that cannot
    /// seal fails instead of skipping.
    const REQUIRE_SANDBOX_ENV: &str = "MUR_TEST_REQUIRE_SANDBOX";

    const EXIT_WRONG_RESULT: i32 = 1;
    const EXIT_CONTROL_FAILED: i32 = 2;
    const EXIT_SSH_WRITABLE: i32 = 3;
    const EXIT_SANDBOX_REQUIRED: i32 = 4;

    fn run_child(mode: &str) -> Option<i32> {
        let root = tempfile::tempdir().unwrap();
        // SBPL matches resolved paths; macOS tempdirs sit behind /var.
        let root = root.path().canonicalize().unwrap();
        Command::new(std::env::current_exe().unwrap())
            .env(ROOT_ENV, &root)
            .env(MODE_ENV, mode)
            .env("HOME", root.join("home"))
            .env_remove("MUR_AGENT_SKIP_SANDBOX")
            .status()
            .unwrap()
            .code()
    }

    /// Under `--full-perms` a path that was in the user's `fs.deny`, inside a
    /// declared write grant, is writable from inside the seal; `~/.ssh` is not.
    #[test]
    fn full_perms_reopens_a_user_deny_but_not_ssh() {
        assert_eq!(
            run_child(MODE_OVERRIDE),
            Some(0),
            "exit {EXIT_WRONG_RESULT}=user deny still enforced, \
             {EXIT_CONTROL_FAILED}=control write failed, \
             {EXIT_SSH_WRITABLE}=~/.ssh writable, \
             {EXIT_SANDBOX_REQUIRED}=sandbox did not seal"
        );
    }

    /// Negative control: the same profile on a normal boot keeps the path
    /// unwritable, so the test above proves the reset and not a no-op.
    #[test]
    fn a_normal_boot_keeps_the_user_deny() {
        assert_eq!(run_child(MODE_NORMAL), Some(0));
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    fn writable(dir: &Path) -> bool {
        let f = dir.join("probe.txt");
        let ok = std::fs::write(&f, b"x").is_ok();
        std::fs::remove_file(&f).ok();
        ok
    }

    fn skip_or_fail_unenforced(why: &dyn std::fmt::Debug) -> ! {
        if std::env::var_os(REQUIRE_SANDBOX_ENV).is_some_and(|v| v == "1") {
            eprintln!("FAIL: {REQUIRE_SANDBOX_ENV}=1 but sandbox not enforcing: {why:?}");
            std::process::exit(EXIT_SANDBOX_REQUIRED);
        }
        eprintln!("SKIP: sandbox not enforcing: {why:?}");
        std::process::exit(0);
    }

    #[ctor::ctor]
    fn full_perms_child_main() {
        let Some(root) = std::env::var_os(ROOT_ENV) else {
            return;
        };
        use mur_agent_runtime::sandbox;
        use mur_common::agent::{AgentProfile, OverrideFlags};

        let override_boot = std::env::var(MODE_ENV).as_deref() == Ok(MODE_OVERRIDE);
        let root = PathBuf::from(root);
        let agent_home = root.join("mur").join("agents").join("tester");
        let work = root.join("work");
        let secrets = work.join("secrets");
        let ssh = root.join("home").join(".ssh");
        for d in [&agent_home, &secrets, &ssh] {
            std::fs::create_dir_all(d).unwrap();
        }

        let mut profile = AgentProfile::default_for_tests();
        let fs = &mut profile.entitlements.filesystem;
        fs.write.push(s(&work));
        fs.deny.push(s(&secrets));
        if override_boot {
            let flags = OverrideFlags {
                full_perms: true,
                ..OverrideFlags::default()
            };
            mur_common::startup_override::merge::apply(&mut profile.entitlements, flags);
            let deny = &profile.entitlements.filesystem.deny;
            assert!(!deny.contains(&s(&secrets)), "precondition: reset");
            assert!(
                deny.iter().any(|d| d == "~/.ssh"),
                "precondition: default kept"
            );
        }

        match sandbox::apply(&profile.entitlements, &agent_home, &[], &[], &[]) {
            Ok(st) if st.enforcing => {}
            other => skip_or_fail_unenforced(&other),
        }

        // Control: the seal is not refusing everything.
        if !writable(&agent_home) {
            eprintln!("ERROR: control write inside agent_home failed; test proves nothing");
            std::process::exit(EXIT_CONTROL_FAILED);
        }
        if writable(&secrets) != override_boot {
            eprintln!(
                "ERROR: previously-denied path writable={} on {} boot",
                !override_boot,
                if override_boot { "override" } else { "normal" }
            );
            std::process::exit(EXIT_WRONG_RESULT);
        }
        if writable(&ssh) {
            eprintln!("ERROR: ~/.ssh writable inside the seal");
            std::process::exit(EXIT_SSH_WRITABLE);
        }
        std::process::exit(0);
    }
}
