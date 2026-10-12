//! Hot reload under a REAL kernel seal (macOS SBPL / Linux Landlock).
//!
//! The manual E2E for #1788 ran with `fail_closed_on_sandbox_error: false`,
//! i.e. advisory mode, so it never proved that a sealed agent can still see
//! its own `profile.yaml` / `config.yaml` change and record what it did. That
//! is exactly what a seal could break: `profile.yaml` is write-denied to the
//! agent (#712) and Landlock reads are allow-list only, so a missing grant
//! would make every reload a silent no-op.
//!
//! Shape: the test re-execs its own binary; a `#[ctor]` in the child seals,
//! says `SEALED`, waits for the unsealed parent to edit both files, then
//! checks the reload from inside the seal. Exit codes name the failure.

#[cfg(unix)]
mod sealed {
    use std::io::{BufRead, Write};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    const CHILD_ENV: &str = "MUR_TEST_SANDBOX_HOT_RELOAD";
    /// Same contract as `sandbox_e2e.rs`: CI sets it to `1` on runners whose
    /// kernel must seal, turning "not enforcing" from a skip into a failure.
    const REQUIRE_SANDBOX_ENV: &str = "MUR_TEST_REQUIRE_SANDBOX";
    const EXIT_SANDBOX_REQUIRED: i32 = 4;
    const EXIT_SKIPPED: i32 = 10;
    const FIXTURE: &str = include_str!("../../mur-common/tests/fixtures/minimal_profile.yaml");
    const AGENT: &str = "bridge_test";
    const SIBLING: &str = "other_agent";
    const READY: &str = "SEALED";
    const GO: &str = "EDITED";

    fn save(path: &Path, text: &str) {
        let tmp = path.with_extension("yaml.tmp");
        std::fs::write(&tmp, text).unwrap();
        std::fs::rename(&tmp, path).unwrap();
    }

    fn agent_home(mur_home: &Path) -> PathBuf {
        mur_home.join("agents").join(AGENT)
    }

    #[test]
    #[cfg_attr(
        target_os = "linux",
        ignore = "issue #1794: Landlock file grant pins the seal-time inode; atomic-rename config.yaml edits are unreadable"
    )]
    fn a_sealed_agent_still_hot_reloads_profile_and_config_limits() {
        let root = tempfile::tempdir().unwrap();
        // SBPL matches resolved paths; macOS tempdirs sit behind /private/var.
        let mur_home = root.path().canonicalize().unwrap().join("mur");
        let home = agent_home(&mur_home);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(mur_home.join("agents").join(SIBLING)).unwrap();
        save(&home.join("profile.yaml"), FIXTURE);
        // Must exist before the seal: Landlock grants only paths present then.
        save(&mur_home.join("config.yaml"), "{}\n");

        let mut child = Command::new(std::env::current_exe().unwrap())
            .env(CHILD_ENV, &mur_home)
            .env_remove("MUR_AGENT_SKIP_SANDBOX")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut out = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        if line.trim() != READY {
            let status = child.wait().unwrap();
            if status.code() == Some(EXIT_SKIPPED) {
                return;
            }
            panic!("child did not seal (said {line:?}, exit {status:?})");
        }

        // The unsealed side — CLI, Hub, `remind` — edits both files.
        save(
            &home.join("profile.yaml"),
            &format!("{FIXTURE}\nlimits: {{ deadline: 0s }}\n"),
        );
        save(&mur_home.join("config.yaml"), "limits: { stuck: 7m }\n");
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, "{GO}").unwrap();
        drop(stdin);

        let status = child.wait().unwrap();
        assert_eq!(
            status.code(),
            Some(0),
            "sealed agent must pick up both edits and record reload_state.json"
        );
        let seen =
            mur_common::agent::ReloadState::load(&home.join(mur_common::agent::RELOAD_STATE_FILE))
                .expect("the sealed child must be able to write reload_state.json");
        assert_eq!(seen.generation, 1, "{seen:?}");
    }

    /// Child half: seal, hand control to the parent, then reload from inside.
    #[ctor::ctor]
    fn sealed_child_main() {
        let Some(mur_home) = std::env::var_os(CHILD_ENV) else {
            return;
        };
        use mur_agent_runtime::live_profile::LiveSettings;
        let home = agent_home(Path::new(&mur_home));
        let boot = mur_agent_runtime::profile::Profile::load(&home)
            .unwrap()
            .inner;
        match mur_agent_runtime::sandbox::apply(&boot.entitlements, &home, &[], &[], &[]) {
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
        // Built after the seal, as the supervisor does (step 4).
        let live = LiveSettings::new(&home, Path::new(&mur_home), boot, Default::default());

        // Control: the seal is real. A sibling agent's home is write-denied by
        // both kernels (SBPL launch chain; outside every Landlock grant), so a
        // pass below cannot mean "nothing was sealed".
        let sibling = Path::new(&mur_home).join("agents").join(SIBLING);
        if std::fs::write(sibling.join("x"), b"x").is_ok() {
            eprintln!("ERROR: sealed child wrote into a sibling agent; seal proves nothing");
            std::process::exit(2);
        }
        // macOS also denies the agent its own profile.yaml at the kernel
        // (#712). Landlock cannot carve a file out of the writable agent_home;
        // there the tool gate holds that line instead.
        #[cfg(target_os = "macos")]
        if std::fs::write(home.join("profile.yaml"), b"evil\n").is_ok() {
            eprintln!("ERROR: sealed child wrote its own profile.yaml");
            std::process::exit(2);
        }

        println!("{READY}");
        std::io::stdout().flush().unwrap();
        let mut go = String::new();
        std::io::stdin().read_line(&mut go).unwrap();
        if go.trim() != GO {
            std::process::exit(3);
        }

        let mut failed = false;
        let profile = live.profile.current();
        if profile.limits.as_ref().and_then(|l| l.deadline.as_deref()) != Some("0s") {
            eprintln!(
                "ERROR: profile.yaml edit not seen under the seal: {:?} / {:?}",
                profile.limits,
                live.profile.state()
            );
            failed = true;
        }
        if live.global_limits.current().stuck.as_deref() != Some("7m") {
            eprintln!(
                "ERROR: config.yaml limits edit not seen under the seal: {:?}",
                live.global_limits.last_error()
            );
            failed = true;
        }
        let state = live.state();
        if state.last_reload_error.is_some() || state.config_limits_error.is_some() {
            eprintln!("ERROR: reload reported an error under the seal: {state:?}");
            failed = true;
        }
        std::process::exit(i32::from(failed));
    }
}
