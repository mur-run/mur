//! `--full-perms` survives a hot reload, under a REAL kernel seal.
//!
//! Spec (startup permission overrides, §Testing): "a hot reload of an
//! unrelated field leaves it writable". The kernel seal cannot change after
//! `sandbox::apply`, so what a reload could break is the in-process view:
//! `LiveProfile` re-reads `profile.yaml`, whose `filesystem.deny` still holds
//! the user deny the override reset. If the reload replaced the merged
//! entitlements with the on-disk ones, every later turn would see the deny
//! again. The supervisor avoids that by layering the reload onto the merged
//! base (`LiveSettings::with_on_disk`); this test checks it from inside the
//! seal, together with the kernel half (path still writable, `~/.ssh` not).
//!
//! Shape: the test re-execs its own binary; a `#[ctor]` in the child loads
//! `profile.yaml`, merges the override, seals, says `SEALED`, waits for the
//! unsealed parent to edit an unrelated field, then reloads and probes.
//! `HOME` points into the tempdir so `~/.ssh` never touches the real one.

#[cfg(unix)]
mod unix {
    use std::io::{BufRead, Write};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    const ROOT_ENV: &str = "MUR_TEST_FULL_PERMS_RELOAD_ROOT";
    /// Same contract as `sandbox_e2e.rs`: CI sets it so a runner that cannot
    /// seal fails instead of skipping.
    const REQUIRE_SANDBOX_ENV: &str = "MUR_TEST_REQUIRE_SANDBOX";
    const FIXTURE: &str = include_str!("../../mur-common/tests/fixtures/minimal_profile.yaml");
    const FIXTURE_FS: &str = r#"filesystem: { read: [], write: [], deny: ["~/.ssh"] }"#;
    /// The fixture's `name`; `Profile::load` refuses a different one.
    const AGENT: &str = "bridge_test";
    const READY: &str = "SEALED";
    const GO: &str = "EDITED";
    /// The unrelated field the parent edits while the child is sealed.
    const EDITED_DEADLINE: &str = "5m";

    const EXIT_RELOAD_NOT_SEEN: i32 = 1;
    const EXIT_CONTROL_FAILED: i32 = 2;
    const EXIT_SSH_WRITABLE: i32 = 3;
    const EXIT_SANDBOX_REQUIRED: i32 = 4;
    const EXIT_RELAXATION_LOST: i32 = 5;
    const EXIT_PROTOCOL: i32 = 6;
    const EXIT_SKIPPED: i32 = 10;

    struct Layout {
        mur_home: PathBuf,
        agent_home: PathBuf,
        work: PathBuf,
        secrets: PathBuf,
        ssh: PathBuf,
    }

    fn layout(root: &Path) -> Layout {
        let mur_home = root.join("mur");
        let work = root.join("work");
        Layout {
            agent_home: mur_home.join("agents").join(AGENT),
            mur_home,
            secrets: work.join("secrets"),
            work,
            ssh: root.join("home").join(".ssh"),
        }
    }

    /// Atomic replace, the way every MUR writer saves YAML.
    fn save(path: &Path, text: &str) {
        let tmp = path.with_extension("yaml.tmp");
        std::fs::write(&tmp, text).unwrap();
        std::fs::rename(&tmp, path).unwrap();
    }

    /// The fixture with a write grant on `work` and a user deny on `secrets`.
    fn profile_yaml(l: &Layout, extra: &str) -> String {
        let fs = format!(
            r#"filesystem: {{ read: [], write: ["{}"], deny: ["~/.ssh", "{}"] }}"#,
            l.work.display(),
            l.secrets.display()
        );
        assert_eq!(FIXTURE.matches(FIXTURE_FS).count(), 1, "fixture drifted");
        format!("{}\n{extra}\n", FIXTURE.replace(FIXTURE_FS, &fs))
    }

    /// Under `--full-perms`, editing an unrelated `profile.yaml` field while
    /// sealed keeps the reset deny path writable and `~/.ssh` unwritable, and
    /// the reloaded profile still carries the merged entitlements.
    #[test]
    fn full_perms_survives_a_hot_reload_of_an_unrelated_field() {
        let root = tempfile::tempdir().unwrap();
        // SBPL matches resolved paths; macOS tempdirs sit behind /var.
        let root = root.path().canonicalize().unwrap();
        let l = layout(&root);
        // Must exist before the seal: Landlock grants only paths present then.
        for d in [&l.agent_home, &l.secrets, &l.ssh] {
            std::fs::create_dir_all(d).unwrap();
        }
        save(&l.agent_home.join("profile.yaml"), &profile_yaml(&l, ""));
        save(&l.mur_home.join("config.yaml"), "{}\n");
        std::fs::create_dir_all(mur_common::limits::global_limits_dir(&l.mur_home)).unwrap();
        save(&mur_common::limits::global_limits_path(&l.mur_home), "{}\n");

        let mut child = Command::new(std::env::current_exe().unwrap())
            .env(ROOT_ENV, &root)
            .env("HOME", root.join("home"))
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

        // The unsealed side edits a hot field; `filesystem.deny` on disk
        // still holds the user deny the override reset.
        save(
            &l.agent_home.join("profile.yaml"),
            &profile_yaml(&l, &format!("limits: {{ deadline: {EDITED_DEADLINE} }}")),
        );
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, "{GO}").unwrap();
        drop(stdin);

        assert_eq!(
            child.wait().unwrap().code(),
            Some(0),
            "exit {EXIT_RELOAD_NOT_SEEN}=edit not reloaded, \
             {EXIT_CONTROL_FAILED}=control failed, \
             {EXIT_SSH_WRITABLE}=~/.ssh writable, \
             {EXIT_SANDBOX_REQUIRED}=sandbox did not seal, \
             {EXIT_RELAXATION_LOST}=override lost after reload"
        );
    }

    fn writable(dir: &Path) -> bool {
        let f = dir.join("probe.txt");
        let ok = std::fs::write(&f, b"x").is_ok();
        std::fs::remove_file(&f).ok();
        ok
    }

    fn fail(code: i32, why: &str) -> ! {
        eprintln!("ERROR: {why}");
        std::process::exit(code);
    }

    #[ctor::ctor]
    fn full_perms_reload_child_main() {
        let Some(root) = std::env::var_os(ROOT_ENV) else {
            return;
        };
        use mur_agent_runtime::live_profile::LiveSettings;
        use mur_common::agent::OverrideFlags;

        let l = layout(Path::new(&root));
        let secrets = l.secrets.to_string_lossy().into_owned();
        // Boot order as in the supervisor: load, merge, seal, then LiveSettings.
        let on_disk = mur_agent_runtime::profile::Profile::load(&l.agent_home)
            .unwrap()
            .inner;
        assert!(
            on_disk.entitlements.filesystem.deny.contains(&secrets),
            "precondition: user deny on disk"
        );
        let mut boot = on_disk.clone();
        let flags = OverrideFlags {
            full_perms: true,
            ..OverrideFlags::default()
        };
        mur_common::startup_override::merge::apply(&mut boot.entitlements, flags);
        assert!(
            !boot.entitlements.filesystem.deny.contains(&secrets),
            "precondition: reset"
        );

        match mur_agent_runtime::sandbox::apply(&boot.entitlements, &l.agent_home, &[], &[], &[]) {
            Ok(st) if st.enforcing => {}
            other => {
                if std::env::var_os(REQUIRE_SANDBOX_ENV).is_some_and(|v| v == "1") {
                    eprintln!("FAIL: {REQUIRE_SANDBOX_ENV}=1 but sandbox not enforcing: {other:?}");
                    std::process::exit(EXIT_SANDBOX_REQUIRED);
                }
                eprintln!("SKIP: sandbox not enforcing: {other:?}");
                std::process::exit(EXIT_SKIPPED);
            }
        }
        let live = LiveSettings::with_on_disk(
            &l.agent_home,
            &l.mur_home,
            boot.clone(),
            Some(on_disk),
            Default::default(),
        );

        // Controls before the reload: the seal is real and the override took.
        if !writable(&l.agent_home) {
            fail(EXIT_CONTROL_FAILED, "write inside agent_home failed");
        }
        if !writable(&l.secrets) {
            fail(EXIT_CONTROL_FAILED, "override did not reopen the user deny");
        }
        if writable(&l.ssh) {
            fail(EXIT_SSH_WRITABLE, "~/.ssh writable before the reload");
        }

        println!("{READY}");
        std::io::stdout().flush().unwrap();
        let mut go = String::new();
        std::io::stdin().read_line(&mut go).unwrap();
        if go.trim() != GO {
            std::process::exit(EXIT_PROTOCOL);
        }

        let p = live.profile.current();
        let state = live.profile.state();
        if live.profile.generation() != 1
            || p.limits.as_ref().and_then(|x| x.deadline.as_deref()) != Some(EDITED_DEADLINE)
            || state.last_reload_error.is_some()
        {
            eprintln!(
                "ERROR: edit not reloaded under the seal: {:?} / {state:?}",
                p.limits
            );
            std::process::exit(EXIT_RELOAD_NOT_SEEN);
        }
        if p.entitlements != boot.entitlements || !state.pending_cold_restart.is_empty() {
            eprintln!(
                "ERROR: reload dropped the merged entitlements: deny={:?} cold={:?}",
                p.entitlements.filesystem.deny, state.pending_cold_restart
            );
            std::process::exit(EXIT_RELAXATION_LOST);
        }
        if !writable(&l.secrets) {
            fail(
                EXIT_RELAXATION_LOST,
                "reset deny path unwritable after reload",
            );
        }
        if writable(&l.ssh) {
            fail(EXIT_SSH_WRITABLE, "~/.ssh writable after the reload");
        }
        std::process::exit(0);
    }
}
