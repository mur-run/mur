use super::*;
use std::path::Path;

const STOPPED: &str = "MainPID=0\nEnvironmentFiles=\nUnsetEnvironment=\n";

/// What a start or restart is told, the verdict most cases check.
fn sd_start(
    show: &str,
    read_environ: impl FnOnce(u32) -> Option<Vec<u8>>,
    manager_env: impl FnOnce() -> Option<String>,
) -> ServiceHome {
    systemd_service_home(show, read_environ, manager_env).for_start()
}

fn no_proc(_: u32) -> Option<Vec<u8>> {
    panic!("a stopped unit has no process environment to read")
}

#[test]
fn running_unit_is_owned_only_when_live_and_next_start_agree() {
    let live = |home: &str| format!("HOME=/home/u\0MUR_HOME={home}\0PATH=/bin\0").into_bytes();

    // Live process and the loaded unit agree: that home.
    let show = "MainPID=42\nEnvironment=MUR_HOME=/data/a\n";
    let got = sd_start(show, |pid| (pid == 42).then(|| live("/data/a")), || None);
    assert_eq!(got, ServiceHome::Known(Some("/data/a".into())));

    // Neither sets one (manager environment empty too): the default home.
    let show = "MainPID=42\nEnvironment=\n";
    let got = sd_start(
        show,
        |_| Some(b"HOME=/h\0".to_vec()),
        || Some("LANG=C\n".into()),
    );
    assert_eq!(got, ServiceHome::Known(None));

    // Regression (review P1): the service runs /data/a, a drop-in moved
    // it to /data/b and was daemon-reloaded but not applied. A restart
    // would serve /data/b, so /data/a must not be told it owns it.
    let show = "MainPID=42\nEnvironment=MUR_HOME=/data/b\n";
    let got = sd_start(show, |_| Some(live("/data/a")), || None);
    assert!(
        matches!(&got, ServiceHome::Unknown(r) if r.contains("/data/b")),
        "{got:?}"
    );

    // Same through the manager environment the unit inherits.
    let show = "MainPID=42\nEnvironment=\n";
    let got = sd_start(
        show,
        |_| Some(live("/data/a")),
        || Some("MUR_HOME=/data/b\n".into()),
    );
    assert!(matches!(got, ServiceHome::Unknown(_)), "{got:?}");

    // Live unreadable, or next start unknowable: never a verdict.
    let blind = sd_start(show, |_| None, || Some(String::new()));
    assert!(matches!(blind, ServiceHome::Unknown(_)), "{blind:?}");
    let show = "MainPID=42\nEnvironmentFiles=/etc/m.env (ignore_errors=no)\n";
    let got = sd_start(show, |_| Some(live("/data/a")), || None);
    assert!(matches!(got, ServiceHome::Unknown(_)), "{got:?}");
}

#[test]
fn stop_targets_the_running_unit_even_when_its_next_start_differs() {
    let live = |home: &str| format!("MUR_HOME={home}\0").into_bytes();
    // Regression (review P2): running for /data/a, a reloaded drop-in
    // moves the next start to /data/b. A restart must be refused, but
    // stopping from /data/a must still go through systemd, else
    // Restart=always brings up the /data/b instance after a SIGTERM.
    let show = "MainPID=42\nEnvironment=MUR_HOME=/data/b\n";
    let homes = systemd_service_home(show, |_| Some(live("/data/a")), || None);
    assert_eq!(
        homes.clone().for_stop(),
        ServiceHome::Known(Some("/data/a".into()))
    );
    assert!(matches!(homes.for_start(), ServiceHome::Unknown(_)));

    // An EnvironmentFile= hides the next start, not the running unit.
    let show = "MainPID=42\nEnvironmentFiles=/etc/m.env (ignore_errors=no)\n";
    let homes = systemd_service_home(show, |_| Some(live("/data/a")), || None);
    assert_eq!(
        homes.clone().for_stop(),
        ServiceHome::Known(Some("/data/a".into()))
    );
    assert!(matches!(homes.for_start(), ServiceHome::Unknown(_)));

    // Nothing running: stop and start see the same next-start answer;
    // an unreadable running instance is never a stop verdict.
    let show = "MainPID=0\nEnvironment=MUR_HOME=/data/b\n";
    let homes = systemd_service_home(show, no_proc, || None);
    assert_eq!(homes.clone().for_stop(), homes.for_start());
    let show = "MainPID=42\nEnvironment=MUR_HOME=/data/a\n";
    let homes = systemd_service_home(show, |_| None, || None);
    assert!(matches!(homes.for_stop(), ServiceHome::Unknown(_)));
}

#[test]
fn dropin_override_of_mur_home_is_the_effective_home() {
    // `systemctl show` merges fragment + drop-ins into one property: the
    // installed default unit sets no MUR_HOME, the drop-in adds it.
    let show = format!("{STOPPED}Environment=PATH=/usr/bin MUR_HOME=/data/alternate\n");
    let got = sd_start(&show, no_proc, || Some("MUR_HOME=/manager\n".into()));
    assert_eq!(got, ServiceHome::Known(Some("/data/alternate".into())));

    // The drop-in's quoted form, as `systemctl show` prints it.
    let show = format!("{STOPPED}Environment=\"MUR_HOME=/data/my mur\"\n");
    let got = sd_start(&show, no_proc, || None);
    assert_eq!(got, ServiceHome::Known(Some("/data/my mur".into())));

    // An empty override still beats the manager environment.
    let show = format!("{STOPPED}Environment=MUR_HOME=\n");
    let got = sd_start(&show, no_proc, || Some("MUR_HOME=/m\n".into()));
    assert_eq!(got, ServiceHome::Known(None));
}

#[test]
fn stopped_unit_without_mur_home_inherits_the_manager_environment() {
    let show = format!("{STOPPED}Environment=PATH=/usr/bin\n");
    let got = sd_start(&show, no_proc, || Some("MUR_HOME=/env/d\n".into()));
    assert_eq!(got, ServiceHome::Known(Some("/env/d".into())));

    let got = sd_start(&show, no_proc, || Some("LANG=C\n".into()));
    assert_eq!(got, ServiceHome::Known(None));

    for unreadable in [None, Some("MUR_HOME=$'/a\\nb'\n".to_string())] {
        let got = sd_start(&show, no_proc, || unreadable);
        assert!(matches!(got, ServiceHome::Unknown(_)), "{got:?}");
    }
}

#[test]
fn overrides_mur_cannot_evaluate_refuse_ownership() {
    let show = "MainPID=0\nEnvironmentFiles=/etc/murmurd.env (ignore_errors=no)\n\
                Environment=MUR_HOME=/x\n";
    let got = sd_start(show, no_proc, || Some(String::new()));
    assert!(matches!(got, ServiceHome::Unknown(_)), "{got:?}");

    let show = "MainPID=0\nUnsetEnvironment=MUR_HOME\nEnvironment=MUR_HOME=/x\n";
    let got = sd_start(show, no_proc, || Some(String::new()));
    assert!(matches!(got, ServiceHome::Unknown(_)), "{got:?}");
}

/// What a start or restart is told about the launchd job.
fn ld_start(
    plist: &str,
    print: Result<Option<String>, String>,
    read_environ: impl FnOnce(u32) -> Option<Vec<String>>,
    domain_env: impl FnOnce() -> Option<String>,
) -> ServiceHome {
    launchd_service_home(plist, print, read_environ, domain_env).for_start()
}

/// Not loaded in launchd: a start bootstraps the plist on disk.
fn unloaded() -> Result<Option<String>, String> {
    Ok(None)
}

fn no_environ(_: u32) -> Option<Vec<String>> {
    panic!("a job that is not running has no process environment to read")
}

/// `launchctl print` of a loaded job: `pid` when running, and the
/// loaded definition's own `environment` block.
fn loaded(pid: Option<u32>, env: &[(&str, &str)]) -> Result<Option<String>, String> {
    let mut p = String::from(
        "gui/501/run.mur.murmurd = {\n\tstate = running\n\
         \tinherited environment = {\n\t\tMUR_HOME => /inherited\n\t}\n\
         \tenvironment = {\n",
    );
    for (k, v) in env {
        p.push_str(&format!("\t\t{k} => {v}\n"));
    }
    p.push_str("\t}\n");
    if let Some(pid) = pid {
        p.push_str(&format!("\tpid = {pid}\n"));
    }
    p.push_str("}\n");
    Ok(Some(p))
}

fn plist(home: &str) -> String {
    format!(
        "<plist><dict>{}</dict></plist>",
        super::super::launchd_env_block(Some(Path::new(home)))
    )
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn unloaded_launchd_job_reads_plist_then_domain_environment() {
    let wrap = |env: &str| format!("<plist><dict>{env}\n</dict></plist>");
    assert_eq!(
        ld_start(&plist("/data/a"), unloaded(), no_environ, || panic!(
            "the plist answers"
        )),
        ServiceHome::Known(Some("/data/a".into()))
    );
    let unset = wrap("<key>Label</key>");
    assert_eq!(
        ld_start(&unset, unloaded(), no_environ, || Some(
            "/setenv/home\n".into()
        )),
        ServiceHome::Known(Some("/setenv/home".into()))
    );
    assert_eq!(
        ld_start(&unset, unloaded(), no_environ, || Some("\n".into())),
        ServiceHome::Known(None)
    );
    for (body, env) in [
        (unset.as_str(), None),
        ("bplist00 binary", Some(String::new())),
        (
            "<plist><key>MUR_HOME</key><data>x</data></plist>",
            Some(String::new()),
        ),
    ] {
        let got = ld_start(body, unloaded(), no_environ, || env);
        assert!(matches!(got, ServiceHome::Unknown(_)), "{got:?}");
    }
}

#[test]
fn loaded_launchd_job_restarts_from_its_loaded_definition_not_the_plist() {
    // Regression (review P2): `kickstart -k` relaunches the LOADED
    // definition. A plist edited on disk to /data/b changes nothing
    // until bootout + bootstrap, so a job loaded with /data/a still
    // restarts as /data/a.
    let got = ld_start(
        &plist("/data/b"),
        loaded(Some(7), &[("MUR_HOME", "/data/a")]),
        |pid| (pid == 7).then(|| strings(&["MUR_HOME=/data/a"])),
        || panic!("the loaded job sets MUR_HOME"),
    );
    assert_eq!(got, ServiceHome::Known(Some("/data/a".into())));

    // Loaded but not running: the loaded definition still answers, and
    // the `inherited environment` block is never mistaken for it.
    let got = ld_start(
        &plist("/data/b"),
        loaded(None, &[("XPC_SERVICE_NAME", "x")]),
        no_environ,
        || Some("\n".into()),
    );
    assert_eq!(got, ServiceHome::Known(None));
}

#[test]
fn running_launchd_job_is_owned_only_when_live_and_next_start_agree() {
    // Running /data/a, but the loaded definition was replaced with
    // /data/b (bootout + bootstrap while a stray instance ran): a
    // restart serves /data/b.
    let homes = launchd_service_home(
        &plist("/data/b"),
        loaded(Some(7), &[("MUR_HOME", "/data/b")]),
        |_| Some(strings(&["MUR_HOME=/data/a"])),
        || None,
    );
    assert_eq!(
        homes.clone().for_stop(),
        ServiceHome::Known(Some("/data/a".into()))
    );
    let start = homes.for_start();
    assert!(
        matches!(&start, ServiceHome::Unknown(r) if r.contains("/data/b")),
        "{start:?}"
    );

    // `launchctl setenv` changed after start; the job sets nothing.
    let got = ld_start(
        "",
        loaded(Some(7), &[]),
        |_| Some(strings(&["PATH=/usr/bin"])),
        || Some("/data/b\n".into()),
    );
    assert!(matches!(got, ServiceHome::Unknown(_)), "{got:?}");
    let got = ld_start(
        "",
        loaded(Some(7), &[]),
        |_| Some(strings(&["PATH=/usr/bin"])),
        || Some("\n".into()),
    );
    assert_eq!(got, ServiceHome::Known(None));

    // Unreadable, undecodable, no environment block, or launchd unable
    // to say: never a verdict, for start or stop.
    let a = &[("MUR_HOME", "/data/a")][..];
    let no_env_block = Ok(Some("gui/501/x = {\n\tpid = 7\n}\n".to_string()));
    for homes in [
        launchd_service_home("", loaded(Some(7), a), |_| None, || None),
        launchd_service_home(
            "",
            loaded(Some(7), a),
            |_| Some(strings(&["MUR_HOME=/a\u{FFFD}b"])),
            || None,
        ),
        launchd_service_home("", Err("launchd said 5".into()), no_environ, || None),
    ] {
        assert!(matches!(homes.clone().for_start(), ServiceHome::Unknown(_)));
        assert!(matches!(homes.for_stop(), ServiceHome::Unknown(_)));
    }
    let got = ld_start(
        "",
        no_env_block,
        |_| Some(strings(&["MUR_HOME=/data/a"])),
        || None,
    );
    assert!(matches!(got, ServiceHome::Unknown(_)), "{got:?}");
}

#[test]
fn launchctl_getenv_keeps_spaces_that_belong_to_the_path() {
    // Regression (review P2): only getenv's trailing newline is framing.
    let bare = "<plist><key>Label</key></plist>";
    for (out, want) in [
        ("/data/mur \n", Some("/data/mur ")),
        (" /data/mur\n", Some(" /data/mur")),
        ("   \n", Some("   ")),
        ("/data/mur", Some("/data/mur")),
        ("\n", None),
    ] {
        let got = ld_start(bare, unloaded(), no_environ, || Some(out.into()));
        assert_eq!(got, ServiceHome::Known(want.map(PathBuf::from)), "{out:?}");
    }
}

#[test]
fn launchctl_print_pid_reads_only_the_jobs_own_line() {
    let print = "gui/501/run.mur.murmurd = {\n\tstate = running\n\
                 \tendpoints = {\n\t\tpid = 1\n\t}\n\tpid = 86318\n}\n";
    assert_eq!(launchctl_print_pid(print), Some(86318));
    let idle = "gui/501/x = {\n\tstate = not running\n\t\tpid = 9\n}\n";
    assert_eq!(launchctl_print_pid(idle), None);
}
