use super::*;

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

fn strict_with_ssh() -> SandboxPolicy {
    SandboxPolicy {
        spawn_mode: SpawnMode::Strict,
        fs_read: vec![p("/usr")],
        fs_write: vec![p("/work")],
        spawn_allowed_paths: vec![p("/bin/bash"), p("/usr/bin/ssh")],
        spawn_allowed_prefixes: vec![p("/opt/tool")],
        ..SandboxPolicy::default()
    }
}

#[test]
fn strict_on_seatbelt_is_the_literals_and_prefixes_only() {
    let r = strict_with_ssh().exec_reach(ExecBackend::Seatbelt);
    assert_eq!(
        r.dirs,
        vec![p("/opt/tool")],
        "no system exec dirs under Strict"
    );
    assert_eq!(r.files, vec![p("/bin/bash"), p("/usr/bin/ssh")]);
    assert!(!r.rw_exec && !r.unrestricted);
}

#[test]
fn strict_on_landlock_gives_read_write_no_exec() {
    let r = strict_with_ssh().exec_reach(ExecBackend::Landlock);
    assert!(
        !r.rw_exec,
        "#2343: Strict read/write grants carry no Execute"
    );
    assert!(!r.dirs.contains(&p("/work")) && !r.dirs.contains(&p("/usr")));
    assert!(r.dirs.contains(&p("/opt/tool")));
    assert!(r.files.contains(&p("/usr/bin/ssh")));
}

#[test]
fn allowlist_differs_from_strict_on_both_backends() {
    let mut pol = strict_with_ssh();
    pol.spawn_mode = SpawnMode::Allowlist;
    let mac = pol.exec_reach(ExecBackend::Seatbelt);
    for d in MACOS_SYSTEM_EXEC_PATHS {
        assert!(mac.dirs.contains(&p(d)), "Allowlist re-allows {d}");
    }
    let lin = pol.exec_reach(ExecBackend::Landlock);
    assert!(lin.rw_exec);
    assert!(
        lin.dirs.contains(&p("/work")),
        "a writable grant is executable"
    );
}

#[test]
fn any_is_unrestricted() {
    let mut pol = strict_with_ssh();
    pol.spawn_mode = SpawnMode::Any;
    assert!(pol.exec_reach(ExecBackend::Landlock).unrestricted);
}

#[test]
fn only_seatbelt_isolates_the_signing_key() {
    assert!(SandboxPolicy::signing_key_isolated(Some(
        ExecBackend::Seatbelt
    )));
    assert!(!SandboxPolicy::signing_key_isolated(Some(
        ExecBackend::Landlock
    )));
    assert!(!SandboxPolicy::signing_key_isolated(None));
}
