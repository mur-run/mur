use super::*;
use crate::cmd::code_nav::smoke_probe::SERVER_PGID_PREFIX;
use std::os::unix::fs::PermissionsExt;

fn manifest(home: &Path, python: bool, php: bool, ts: bool) -> Manifest {
    let tools = node_ls_install::tools_dir(home);
    let mut m = Manifest::default();
    if python {
        m.pyright = Some(super::super::pyright_install::Record {
            version: "1.1.403".into(),
            exclude_newer: String::new(),
            bin: home.join("tools/pyright/bin/pyright-langserver"),
        });
    }
    if php {
        m.intelephense = Some(node_ls_install::Record::for_tools(
            &serena_ls_cmd::INTELEPHENSE,
            &tools,
        ));
    }
    if ts {
        m.typescript = Some(node_ls_install::Record::for_tools(
            &serena_ls_cmd::TYPESCRIPT,
            &tools,
        ));
    }
    m
}

#[test]
fn targets_are_the_managed_installs_launched_as_serena_would() {
    let home = Path::new("/h/.mur");
    let m = manifest(home, true, true, true);
    let langs = [Lang::Python, Lang::Php, Lang::Lua, Lang::TypeScript];
    let t = targets(&m, &langs, home);
    let tools = node_ls_install::tools_dir(home);
    let mut ts = serena_ls_cmd::canonical_base_cmd(serena_ls_cmd::TS_LS_ID, &tools).unwrap();
    ts.push("--stdio".into());
    assert_eq!(t.len(), 3, "lua is serena-fetched, not launched: {t:?}");
    assert_eq!(
        t[0].cmd,
        ["/h/.mur/tools/pyright/bin/pyright-langserver", "--stdio"]
    );
    assert_eq!(t[1].lang, Lang::Php);
    assert_eq!(t[1].cmd[0], "node");
    assert_eq!(
        t[2],
        Target {
            lang: Lang::TypeScript,
            cmd: ts
        }
    );
}

/// A language enabled for the entry but not pre-installed has nothing to
/// launch; it must not become a target pointing at a missing file.
#[test]
fn an_enabled_language_without_an_install_is_not_a_target() {
    let home = Path::new("/h/.mur");
    let t = targets(
        &manifest(home, false, false, false),
        &[Lang::Python, Lang::Php],
        home,
    );
    assert!(t.is_empty(), "{t:?}");
}

#[test]
fn the_last_parseable_stdout_line_is_the_report() {
    let out = b"some log line\n{\"status\":\"ok\"}\n";
    assert_eq!(parse_report(out, b""), Report::Ok);
}

#[test]
fn a_probe_that_died_reports_its_last_stderr_line() {
    let r = parse_report(b"", b"warning: x\nthread panicked at foo\n\n");
    assert_eq!(
        r,
        Report::Failed {
            detail: "the smoke probe died without a report: thread panicked at foo".into()
        }
    );
}

#[test]
fn nothing_to_launch_prints_nothing() {
    let mut out = Vec::new();
    verify("a", Path::new("/w"), &[], &[], Path::new("/f"), &mut out).unwrap();
    assert!(out.is_empty());
}

#[test]
fn only_unmanaged_languages_succeeds_and_says_so() {
    let mut out = Vec::new();
    verify(
        "a",
        Path::new("/w"),
        &[],
        &[(
            Lang::Lua,
            "serena could not fetch it during setup: offline".into(),
        )],
        Path::new("/f"),
        &mut out,
    )
    .unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains("lua: not verified — serena could not fetch it"),
        "{out}"
    );
    assert!(out.contains("serena will not start"), "{out}");
}

#[test]
fn fetch_dir_is_under_the_entry_home() {
    let home = Path::new("/a/serena-homes/repo");
    assert_eq!(
        fetch_dir(home),
        Path::new("/a/serena-homes/repo/language_servers/static")
    );
}

/// #1993: the unverified warning names the exact grant serena's download
/// directory needs, and stays quiet once it is granted.
#[test]
fn fetch_grant_names_the_download_dir_only_when_refused() {
    let dir = Path::new("/a/serena-homes/repo/language_servers/static");
    let hint = fetch_grant("si-te", dir, |_| false).unwrap();
    assert!(
        hint.contains(&format!(
            "mur agent perm allow-spawn-dir si-te {}",
            dir.display()
        )),
        "{hint}"
    );
    assert_eq!(fetch_grant("si-te", dir, |_| true), None);
}

/// The hint names the directory the seal refuses — the server's own and its
/// `#!` interpreter's — so the user can grant exactly that.
#[test]
fn blocked_dirs_name_the_binary_and_its_shebang_interpreter() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let py_dir = root.join("venv/bin");
    let ls_dir = root.join("ls/bin");
    std::fs::create_dir_all(&py_dir).unwrap();
    std::fs::create_dir_all(&ls_dir).unwrap();
    let py = py_dir.join("python");
    std::fs::write(&py, "").unwrap();
    let ls = ls_dir.join("pyright-langserver");
    std::fs::write(&ls, format!("#!{}\nimport sys\n", py.display())).unwrap();
    std::fs::set_permissions(&ls, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cmd = vec![ls.to_string_lossy().into_owned(), "--stdio".into()];
    let path = std::ffi::OsString::from("/usr/bin");

    let all = blocked_dirs(&cmd, &path, |_| false);
    assert_eq!(all, vec![ls_dir.clone(), py_dir.clone()]);

    let only_py = blocked_dirs(&cmd, &path, |p| p.starts_with(&ls_dir));
    assert_eq!(only_py, vec![py_dir]);

    assert!(blocked_dirs(&cmd, &path, |_| true).is_empty());
}

fn target(lang: Lang) -> Target {
    Target {
        lang,
        cmd: vec!["x".into()],
    }
}

/// #1981: "Setup reports success only if this passes." A launch that could
/// not be verified is not a pass — in an already-sealed shell or under a
/// non-enforcing seal alike.
#[test]
fn an_unverified_launch_fails_setup() {
    for detail in [
        "this shell already runs inside a sandbox",
        "the agent's sandbox is not enforcing on this host (unsupported)",
    ] {
        let mut out = Vec::new();
        let err = verify_with(
            "a",
            &[target(Lang::TypeScript)],
            &[],
            Path::new("/f"),
            &mut out,
            |_| Report::Unverified {
                detail: detail.into(),
            },
        )
        .expect_err("unverified must not be success");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("typescript") && msg.contains("not verified"),
            "{msg}"
        );
        assert!(
            msg.contains("kept"),
            "installs and grants are preserved: {msg}"
        );
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains(detail), "{out}");
    }
}

#[test]
fn every_launch_ok_is_success() {
    let mut out = Vec::new();
    verify_with(
        "a",
        &[target(Lang::Php), target(Lang::TypeScript)],
        &[],
        Path::new("/f"),
        &mut out,
        |_| Report::Ok,
    )
    .unwrap();
}

#[test]
fn failed_and_unverified_are_both_named() {
    let mut out = Vec::new();
    let err = verify_with(
        "a",
        &[target(Lang::Php), target(Lang::TypeScript)],
        &[],
        Path::new("/f"),
        &mut out,
        |t| match t.lang {
            Lang::Php => Report::Failed {
                detail: "boom".into(),
            },
            _ => Report::Unverified {
                detail: "sealed".into(),
            },
        },
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("php") && msg.contains("typescript"), "{msg}");
}

/// The parent bounds the probe independently of the probe's own timeout.
#[test]
fn a_hung_probe_is_stopped_at_the_parent_deadline() {
    let mut c = Command::new("/bin/sh");
    c.arg("-c").arg("sleep 30 & exec sleep 30");
    let child = server_command(&mut c).spawn().unwrap();
    let start = Instant::now();
    let r = collect(child, Duration::from_millis(300));
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "{:?}",
        start.elapsed()
    );
    assert!(
        matches!(&r, Report::Failed { detail } if detail.contains("did not finish")),
        "{r:?}"
    );
}

/// Whether process group `pgid` still has a live member, polled briefly so a
/// just-killed member has time to be reaped.
fn group_alive(pgid: libc::pid_t) -> bool {
    for _ in 0..40 {
        // SAFETY: signal 0 only probes for existence.
        if unsafe { libc::killpg(pgid, 0) } != 0 {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

/// The real topology: the probe and the server it starts each lead their own
/// process group (both go through `server_command`). A probe that hangs past
/// the parent deadline never runs its own cleanup, so the parent must take
/// the server's group down too.
#[test]
fn a_hung_probe_takes_its_separately_grouped_server_down() {
    let tmp = tempfile::tempdir().unwrap();
    let pgid_file = tmp.path().join("server.pgid");
    let mut c = Command::new("/bin/sh");
    // `set -m` gives the background job a process group of its own, as
    // `process_group(0)` does for the real server.
    c.arg("-c").arg(format!(
        "set -m; sleep 30 & echo $! > '{f}'; echo \"{p}$!\"; exec sleep 30",
        f = pgid_file.display(),
        p = SERVER_PGID_PREFIX,
    ));
    let child = server_command(&mut c).spawn().unwrap();
    let probe_pgid = child.id() as libc::pid_t;
    let r = collect(child, Duration::from_millis(500));
    assert!(
        matches!(&r, Report::Failed { detail } if detail.contains("did not finish")),
        "{r:?}"
    );
    let server_pgid: libc::pid_t = std::fs::read_to_string(&pgid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_ne!(server_pgid, probe_pgid, "the fake must use two groups");
    let probe_gone = !group_alive(probe_pgid);
    let server_gone = !group_alive(server_pgid);
    if !server_gone {
        // SAFETY: cleanup of the group this test created.
        unsafe { libc::killpg(server_pgid, libc::SIGKILL) };
    }
    assert!(probe_gone, "probe group {probe_pgid} survived");
    assert!(server_gone, "server group {server_pgid} survived");
}

/// A probe that dies after starting the server (crash, signal, early exit)
/// never runs its own cleanup either, so an abnormal exit must take the
/// announced server group down just like a hang does.
#[test]
fn a_probe_that_exits_abnormally_takes_its_server_down() {
    let tmp = tempfile::tempdir().unwrap();
    let pgid_file = tmp.path().join("server.pgid");
    let mut c = Command::new("/bin/sh");
    // The server's stdio goes to /dev/null so the probe's pipes close on exit,
    // as the real server's do (its stdio is piped to the probe, not shared).
    c.arg("-c").arg(format!(
        "set -m; sleep 30 </dev/null >/dev/null 2>&1 & echo $! > '{f}'; echo \"{p}$!\"; exit 3",
        f = pgid_file.display(),
        p = SERVER_PGID_PREFIX,
    ));
    let child = server_command(&mut c).spawn().unwrap();
    let probe_pgid = child.id() as libc::pid_t;
    let r = collect(child, Duration::from_secs(10));
    assert!(matches!(&r, Report::Failed { .. }), "{r:?}");
    let server_pgid: libc::pid_t = std::fs::read_to_string(&pgid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_ne!(server_pgid, probe_pgid, "the fake must use two groups");
    let server_gone = !group_alive(server_pgid);
    if !server_gone {
        // SAFETY: cleanup of the group this test created.
        unsafe { libc::killpg(server_pgid, libc::SIGKILL) };
    }
    assert!(server_gone, "server group {server_pgid} survived");
}

#[test]
fn only_a_real_announced_group_is_taken() {
    assert_eq!(parse_server_group("server-pgid: 4242\n"), Some(4242));
    // 0 and 1 would be our own group and init's; never signal those.
    assert_eq!(parse_server_group("server-pgid: 0"), None);
    assert_eq!(parse_server_group("server-pgid: 1"), None);
    assert_eq!(parse_server_group(r#"{"status":"ok"}"#), None);
}

/// The announce line precedes the report and must not hide it.
#[test]
fn the_announce_line_does_not_mask_the_report() {
    let out = format!("{SERVER_PGID_PREFIX}4242\n{{\"status\":\"ok\"}}\n");
    assert_eq!(parse_report(out.as_bytes(), b""), Report::Ok);
}

#[test]
fn a_finished_probe_is_parsed() {
    let mut c = Command::new("/bin/sh");
    c.arg("-c").arg(r#"echo '{"status":"ok"}'"#);
    let child = server_command(&mut c).spawn().unwrap();
    assert_eq!(collect(child, Duration::from_secs(10)), Report::Ok);
}
