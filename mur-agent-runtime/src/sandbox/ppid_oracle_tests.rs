use super::*;
use std::io::Cursor;

fn fake(pid: u32) -> Option<u32> {
    (pid == 42).then_some(7)
}

#[test]
fn serve_handshakes_then_answers_each_line() {
    let mut out = Vec::new();
    serve(Cursor::new("42\n99\nnot-a-pid\n"), &mut out, fake).unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!("{READY}\n7\n{UNKNOWN}\n{UNKNOWN}\n")
    );
}

/// A stand-in helper: banner noise, the handshake, then pid+1 per line.
fn shell_helper(script: &str) -> Command {
    let mut c = Command::new("sh");
    c.args(["-c", script]);
    c
}

#[test]
fn client_skips_noise_before_the_handshake_and_round_trips() {
    let o = Oracle::spawn(shell_helper(&format!(
        "echo 'running 1 test'; echo {READY}; while read p; do echo $((p+1)); done"
    )))
    .unwrap();
    assert_eq!(o.parent_of(10), Some(11));
    assert_eq!(o.parent_of(41), Some(42));
}

#[test]
fn handshake_sharing_a_line_with_unterminated_noise_is_found() {
    let o = Oracle::spawn(shell_helper(&format!(
        "printf 'test x ... '; echo {READY}; while read p; do echo $((p+1)); done"
    )))
    .unwrap();
    assert_eq!(o.parent_of(1), Some(2));
}

#[test]
fn unknown_answer_is_none() {
    let o = Oracle::spawn(shell_helper(&format!(
        "echo {READY}; while read p; do echo {UNKNOWN}; done"
    )))
    .unwrap();
    assert_eq!(o.parent_of(10), None);
}

#[test]
fn helper_that_never_handshakes_is_an_error() {
    assert!(Oracle::spawn(shell_helper("echo nope")).is_err());
}

#[test]
fn dead_helper_fails_closed() {
    let o = Oracle::spawn(shell_helper(&format!("echo {READY}"))).unwrap();
    assert_eq!(o.parent_of(10), None);
}

/// Real kernel: after a Landlock seal the direct procfs read of another pid
/// is denied (control), and the oracle started before the seal still
/// answers. Sealing is irreversible, so the sealed half is a re-exec of this
/// test binary; the oracle helper is a second re-exec.
#[cfg(target_os = "linux")]
mod sealed {
    use super::*;

    const ROLE_ENV: &str = "MUR_PPID_ORACLE_TEST_ROLE";
    const EXIT_OK: i32 = 20;
    const EXIT_NOT_ENFORCING: i32 = 21;
    const EXIT_CONTROL_NOT_DENIED: i32 = 22;
    const EXIT_ORACLE_WRONG: i32 = 23;
    const EXIT_SETUP: i32 = 24;
    const NAME: &str = "sandbox::ppid_oracle::tests::sealed::oracle_answers_after_the_seal";

    fn reexec(role: &str) -> Command {
        let mut c = Command::new(std::env::current_exe().unwrap());
        c.args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env(ROLE_ENV, role);
        c
    }

    fn sealed_child() -> ! {
        // Our parent is the test harness; its parent is whoever ran it.
        let parent = std::os::unix::process::parent_id();
        let Some(want) = read_ppid(parent) else {
            std::process::exit(EXIT_SETUP);
        };
        let Ok(oracle) = Oracle::spawn(reexec("helper")) else {
            std::process::exit(EXIT_SETUP);
        };
        let policy = crate::sandbox::SandboxPolicy::default();
        match crate::sandbox::linux::apply_linux(&policy) {
            Ok(s) if s.enforcing => {}
            Ok(_) => std::process::exit(EXIT_NOT_ENFORCING),
            Err(_) => std::process::exit(EXIT_SETUP),
        }
        if read_ppid(parent).is_some() {
            std::process::exit(EXIT_CONTROL_NOT_DENIED);
        }
        if oracle.parent_of(parent) != Some(want) {
            std::process::exit(EXIT_ORACLE_WRONG);
        }
        std::process::exit(EXIT_OK);
    }

    #[test]
    fn oracle_answers_after_the_seal() {
        match std::env::var(ROLE_ENV).as_deref() {
            Ok("helper") => {
                let _ = serve(io::stdin().lock(), io::stdout().lock(), read_ppid);
                std::process::exit(0);
            }
            Ok("sealed") => sealed_child(),
            _ => {}
        }
        let code = reexec("sealed").status().unwrap().code();
        assert_ne!(
            code,
            Some(EXIT_NOT_ENFORCING),
            "no Landlock; proves nothing"
        );
        assert_ne!(
            code,
            Some(EXIT_CONTROL_NOT_DENIED),
            "seal did not deny /proc/<pid>/stat"
        );
        assert_eq!(code, Some(EXIT_OK), "oracle did not answer after the seal");
    }
}
