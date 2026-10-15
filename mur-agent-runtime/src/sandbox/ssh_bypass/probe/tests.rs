use super::*;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// A stand-in probe child: a shell script that drains stdin (the profile)
/// and then behaves as `body` says. This is the true process boundary the
/// parent talks to, so it is the right thing to fake.
fn fake_child(dir: &Path, body: &str) -> PathBuf {
    let p = dir.join("fake-probe");
    std::fs::write(&p, format!("#!/bin/sh\ncat >/dev/null\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn run(body: &str, timeout: Duration) -> ProbeOutcome {
    let dir = tempfile::TempDir::new().unwrap();
    let exe = fake_child(dir.path(), body);
    run_probe(&exe, "(version 1)", Path::new("/nonexistent.sock"), timeout)
}

// ---- outcome → classification (the three spec results) ----

#[test]
fn denied_is_the_only_outcome_that_claims_boundary() {
    assert_eq!(
        ProbeOutcome::Denied.classification(),
        Classification::Boundary
    );
}

#[test]
fn connected_is_audit_only_probe_reachable() {
    assert_eq!(
        ProbeOutcome::Connected.classification(),
        Classification::AuditOnly(AuditReason::ProbeReachable)
    );
}

#[test]
fn inconclusive_is_audit_only_probe_inconclusive() {
    assert_eq!(
        ProbeOutcome::Inconclusive("x".into()).classification(),
        Classification::AuditOnly(AuditReason::ProbeInconclusive)
    );
}

// ---- parent: child exit → outcome ----

#[test]
fn child_reporting_denied_maps_to_denied() {
    assert_eq!(
        run(&format!("exit {EXIT_DENIED}"), PROBE_TIMEOUT),
        ProbeOutcome::Denied
    );
}

#[test]
fn child_reporting_connected_maps_to_connected() {
    assert_eq!(
        run(&format!("exit {EXIT_CONNECTED}"), PROBE_TIMEOUT),
        ProbeOutcome::Connected
    );
}

#[test]
fn child_reporting_other_errno_is_inconclusive() {
    assert!(matches!(
        run(&format!("exit {EXIT_OTHER_ERRNO}"), PROBE_TIMEOUT),
        ProbeOutcome::Inconclusive(_)
    ));
}

#[test]
fn child_that_could_not_apply_the_sandbox_is_inconclusive_not_connected() {
    // An unsandboxed connect proves nothing about the profile.
    assert!(matches!(
        run(&format!("exit {EXIT_SANDBOX_FAILED}"), PROBE_TIMEOUT),
        ProbeOutcome::Inconclusive(_)
    ));
}

#[test]
fn unknown_exit_code_is_inconclusive() {
    assert!(matches!(
        run("exit 77", PROBE_TIMEOUT),
        ProbeOutcome::Inconclusive(_)
    ));
}

#[test]
fn child_killed_by_signal_is_inconclusive() {
    assert!(matches!(
        run("kill -9 $$", PROBE_TIMEOUT),
        ProbeOutcome::Inconclusive(_)
    ));
}

#[test]
fn child_that_hangs_times_out_as_inconclusive_and_is_bounded() {
    let timeout = Duration::from_millis(300);
    let start = Instant::now();
    let outcome = run("sleep 30", timeout);
    let elapsed = start.elapsed();
    assert!(
        matches!(outcome, ProbeOutcome::Inconclusive(_)),
        "{outcome:?}"
    );
    assert!(
        elapsed < timeout + Duration::from_secs(2),
        "probe must not outlive its timeout by much: {elapsed:?}"
    );
}

#[test]
fn probe_that_cannot_spawn_is_inconclusive_never_boundary() {
    let outcome = run_probe(
        Path::new("/nonexistent/mur-probe-binary"),
        "(version 1)",
        Path::new("/nonexistent.sock"),
        PROBE_TIMEOUT,
    );
    assert!(
        matches!(outcome, ProbeOutcome::Inconclusive(_)),
        "{outcome:?}"
    );
    assert_ne!(outcome.classification(), Classification::Boundary);
}

#[test]
fn parent_passes_profile_on_stdin_and_upstream_as_argv() {
    let dir = tempfile::TempDir::new().unwrap();
    let seen = dir.path().join("seen");
    let p = dir.path().join("fake-probe");
    std::fs::write(
        &p,
        format!(
            "#!/bin/sh\ncat > '{s}.profile'\nprintf '%s\\n' \"$@\" > '{s}.argv'\nexit {EXIT_DENIED}\n",
            s = seen.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    let up = Path::new("/some/upstream.sock");
    assert_eq!(
        run_probe(&p, "(version 1)(allow default)", up, PROBE_TIMEOUT),
        ProbeOutcome::Denied
    );
    let profile = std::fs::read_to_string(dir.path().join("seen.profile")).unwrap();
    let argv = std::fs::read_to_string(dir.path().join("seen.argv")).unwrap();
    assert_eq!(profile, "(version 1)(allow default)");
    assert_eq!(argv, format!("{PROBE_SUBCOMMAND}\n/some/upstream.sock\n"));
}

// ---- child: connect result → exit code ----

#[test]
fn eperm_and_eacces_mean_denied() {
    for errno in [libc::EPERM, libc::EACCES] {
        assert_eq!(
            exit_code_for_connect(&Err(io::Error::from_raw_os_error(errno))),
            EXIT_DENIED
        );
    }
}

#[test]
fn other_errnos_are_not_denied() {
    for errno in [libc::ECONNREFUSED, libc::ENOENT, libc::ETIMEDOUT] {
        assert_eq!(
            exit_code_for_connect(&Err(io::Error::from_raw_os_error(errno))),
            EXIT_OTHER_ERRNO
        );
    }
}

#[test]
fn successful_connect_means_connected() {
    assert_eq!(exit_code_for_connect(&Ok(())), EXIT_CONNECTED);
}

#[test]
fn connect_only_reaches_a_live_listener_and_sends_nothing() {
    let dir = tempfile::TempDir::new().unwrap();
    let sock = dir.path().join("up.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    assert_eq!(connect_only(&sock), EXIT_CONNECTED);
    let (mut conn, _) = listener.accept().unwrap();
    let mut buf = Vec::new();
    io::Read::read_to_end(&mut conn, &mut buf).unwrap();
    assert!(
        buf.is_empty(),
        "probe must not send protocol bytes: {buf:?}"
    );
}

#[test]
fn connect_only_to_a_missing_socket_is_other_errno() {
    let dir = tempfile::TempDir::new().unwrap();
    assert_eq!(
        connect_only(&dir.path().join("missing.sock")),
        EXIT_OTHER_ERRNO
    );
}
