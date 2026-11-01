use super::*;

#[test]
fn failed_stop_of_unloaded_job_is_success() {
    assert!(stop_outcome(anyhow::anyhow!("not loaded"), || Ok(false)).is_ok());
}

#[test]
fn failed_stop_of_loaded_job_is_an_error() {
    let err = stop_outcome(anyhow::anyhow!("bootout denied"), || Ok(true)).unwrap_err();
    assert!(err.to_string().contains("bootout denied"));
}

#[test]
fn failed_stop_with_failed_probe_is_an_error() {
    let err = stop_outcome(anyhow::anyhow!("bootout denied"), || {
        Err(anyhow::anyhow!("probe crashed"))
    })
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("bootout denied") && msg.contains("probe crashed"));
}

#[test]
fn service_units_carry_mur_home_only_when_set() {
    assert_eq!(launchd_env_block(None), "");
    assert_eq!(systemd_env_line(None), "");
    let b = launchd_env_block(Some(Path::new("/data/a&b")));
    assert!(b.contains("<key>MUR_HOME</key>"), "{b}");
    assert!(b.contains("<string>/data/a&amp;b</string>"), "{b}");
    assert_eq!(
        systemd_env_line(Some(Path::new("/data/my mur"))),
        "Environment=\"MUR_HOME=/data/my mur\"\n"
    );
}

#[test]
fn service_home_round_trips_through_installed_units() {
    let plist = format!(
        "<dict>{}\n</dict>",
        launchd_env_block(Some(Path::new("/data/a&b")))
    );
    assert_eq!(
        launchd_plist_mur_home(&plist),
        Some(PathBuf::from("/data/a&b"))
    );
    assert_eq!(
        launchd_plist_mur_home("<dict><key>Label</key></dict>"),
        None
    );

    let unit = format!(
        "[Service]\nExecStart=/bin/murmurd\n{}Restart=always\n",
        systemd_env_line(Some(Path::new("/data/my \"mur\"")))
    );
    assert_eq!(
        service_home::systemd_env_mur_home(&unit).as_deref(),
        Some("/data/my \"mur\"")
    );
    assert_eq!(
        service_home::systemd_env_mur_home("Environment=FOO=1 MUR_HOME=/x\n").as_deref(),
        Some("/x")
    );
    assert_eq!(
        service_home::systemd_env_mur_home("[Service]\nExecStart=/bin/murmurd\n"),
        None
    );
}

#[test]
fn service_for_another_mur_home_is_not_routed_through() {
    let s = DaemonService::Launchd;
    assert_eq!(
        Supervision::classify(
            s,
            PathBuf::from("/nonexistent/u/.mur"),
            Path::new("/nonexistent/u/.mur/")
        ),
        Supervision::Owns(s)
    );
    assert_eq!(
        Supervision::classify(
            s,
            PathBuf::from("/nonexistent/u/.mur"),
            Path::new("/nonexistent/alt")
        ),
        Supervision::Other {
            service: s,
            service_home: PathBuf::from("/nonexistent/u/.mur")
        }
    );
}

#[test]
fn systemd_ownership_is_liveness_or_enablement_not_enablement_alone() {
    let show = |load: &str, active: &str, file: &str| {
        format!("LoadState={load}\nActiveState={active}\nUnitFileState={file}\nFragmentPath=/u\n")
    };
    // Disabled while running: Restart=always still respawns it.
    assert!(systemd_owns_unit(&show("loaded", "active", "disabled")));
    // Between respawns (SubState=auto-restart).
    assert!(systemd_owns_unit(&show("loaded", "activating", "disabled")));
    assert!(systemd_owns_unit(&show("loaded", "inactive", "enabled")));
    assert!(systemd_owns_unit(&show(
        "loaded",
        "inactive",
        "enabled-runtime"
    )));
    assert!(!systemd_owns_unit(&show("loaded", "inactive", "disabled")));
    assert!(!systemd_owns_unit(&show("loaded", "failed", "disabled")));
    assert!(!systemd_owns_unit(&show("not-found", "inactive", "")));
}

#[test]
fn service_start_argv_restarts_in_place() {
    let t = "gui/501/run.mur.murmurd";
    assert_eq!(
        DaemonService::Launchd.start_argv(true, t),
        ["launchctl", "kickstart", "-k", t]
    );
    assert_eq!(
        DaemonService::Launchd.start_argv(false, t),
        ["launchctl", "kickstart", t]
    );
    assert_eq!(
        DaemonService::Systemd.start_argv(true, t),
        ["systemctl", "--user", "restart", SYSTEMD_UNIT]
    );
    assert_eq!(
        DaemonService::Systemd.start_argv(false, t),
        ["systemctl", "--user", "start", SYSTEMD_UNIT]
    );
}

#[test]
fn service_stop_goes_through_the_service_manager() {
    let t = "gui/501/run.mur.murmurd";
    assert_eq!(
        DaemonService::Launchd.stop_argv(t),
        ["launchctl", "bootout", t]
    );
    assert_eq!(
        DaemonService::Systemd.stop_argv(t),
        ["systemctl", "--user", "stop", SYSTEMD_UNIT]
    );
}

#[test]
fn launchd_start_falls_back_to_bootstrap_because_stop_unloads_the_job() {
    let plist = Path::new("/h/Library/LaunchAgents/run.mur.murmurd.plist");
    // `bootstrap`, not `load -w`: the latter exits 0 on "Load failed".
    assert_eq!(
        DaemonService::Launchd
            .load_fallback_argv("gui/501", plist)
            .unwrap(),
        [
            "launchctl",
            "bootstrap",
            "gui/501",
            "/h/Library/LaunchAgents/run.mur.murmurd.plist"
        ]
    );
    assert!(
        DaemonService::Systemd
            .load_fallback_argv("gui/501", plist)
            .is_none()
    );
}

#[test]
fn launchd_install_reloads_with_bootout_then_bootstrap() {
    let plist = Path::new("/h/Library/LaunchAgents/run.mur.murmurd.plist");
    let [out, boot] = launchd_reload_argvs("gui/501", plist);
    assert_eq!(out, ["launchctl", "bootout", "gui/501/run.mur.murmurd"]);
    assert_eq!(
        boot,
        [
            "launchctl",
            "bootstrap",
            "gui/501",
            "/h/Library/LaunchAgents/run.mur.murmurd.plist"
        ]
    );
}

#[test]
fn launchd_target_names_the_installed_label() {
    let t = launchd_target();
    assert!(
        t.starts_with("gui/") && t.ends_with("/run.mur.murmurd"),
        "{t}"
    );
}

#[test]
fn murmurd_bin_path_returns_path() {
    // Should never panic and should produce a path ending in "murmurd"
    let p = murmurd_bin_path();
    let name = p.file_name().unwrap_or_default().to_string_lossy();
    assert!(
        name.contains("murmurd"),
        "expected murmurd in path, got: {p:?}"
    );
}

#[test]
fn murmurd_bin_path_does_not_panic() {
    // Test that murmurd_bin_path() can be called without panicking.
    let _ = murmurd_bin_path();
}
