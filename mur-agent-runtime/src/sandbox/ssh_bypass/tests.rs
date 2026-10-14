use super::*;
use std::fs;

/// A real file standing in for the upstream socket: classification only
/// resolves and compares the path, it never connects.
fn upstream_in(dir: &Path) -> PathBuf {
    let p = dir.join("agent.sock");
    fs::write(&p, b"").unwrap();
    p
}

fn restricted() -> Option<&'static [u16]> {
    Some(&[])
}

fn run(
    platform: Platform,
    upstream: &Path,
    ports: Option<&[u16]>,
    allow: &[PathBuf],
) -> Classification {
    classify(&Inputs {
        platform,
        upstream,
        net_allow_ports: ports,
        allowlist: allow,
    })
}

fn audit(reason: Reason) -> Classification {
    Classification::AuditOnly(reason)
}

#[test]
fn row1_windows_and_other_platforms_are_audit_only() {
    let dir = tempfile::tempdir().unwrap();
    let up = upstream_in(dir.path());
    for p in [Platform::Windows, Platform::Other] {
        assert_eq!(
            run(p, &up, restricted(), &[]),
            audit(Reason::PlatformNoFsSandbox)
        );
    }
}

#[test]
fn row2_linux_is_audit_only_even_when_everything_else_would_pass() {
    let dir = tempfile::tempdir().unwrap();
    let up = upstream_in(dir.path());
    assert_eq!(
        run(Platform::Linux, &up, restricted(), &[]),
        audit(Reason::PlatformNoUnixSocketControl)
    );
}

#[test]
fn row3_unrestricted_network_is_audit_only() {
    let dir = tempfile::tempdir().unwrap();
    let up = upstream_in(dir.path());
    assert_eq!(
        run(Platform::MacOs, &up, None, &[]),
        audit(Reason::UnrestrictedNetwork)
    );
}

#[test]
fn row3_any_port_list_counts_as_restricted() {
    let dir = tempfile::tempdir().unwrap();
    let up = upstream_in(dir.path());
    let ports: &[u16] = &[443];
    assert!(matches!(
        run(Platform::MacOs, &up, Some(ports), &[]),
        Classification::Candidate { .. }
    ));
}

#[test]
fn row4_missing_upstream_is_audit_only_never_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no-such.sock");
    assert_eq!(
        run(Platform::MacOs, &missing, restricted(), &[]),
        audit(Reason::UpstreamUnresolved)
    );
}

#[test]
fn row5_upstream_under_an_allowlist_root_names_that_root() {
    let allowed = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let nested = allowed.path().join("deep");
    fs::create_dir(&nested).unwrap();
    let up = upstream_in(&nested);
    let root = allowed.path().canonicalize().unwrap();
    let allow = vec![other.path().canonicalize().unwrap(), root.clone()];
    assert_eq!(
        run(Platform::MacOs, &up, restricted(), &allow),
        audit(Reason::UpstreamInAllowlist(root))
    );
}

#[test]
fn row5_allowlist_root_given_unresolved_still_matches_resolved_upstream() {
    // Mirrors `/tmp` → `/private/tmp`: the root is spelled through a symlink.
    let base = tempfile::tempdir().unwrap();
    let real = base.path().join("real");
    fs::create_dir(&real).unwrap();
    let link = base.path().join("link");
    if !make_dir_symlink(&real, &link) {
        return;
    }
    let up = upstream_in(&real);
    assert_eq!(
        run(
            Platform::MacOs,
            &up,
            restricted(),
            std::slice::from_ref(&link)
        ),
        audit(Reason::UpstreamInAllowlist(link))
    );
}

#[test]
fn row5_a_sibling_with_a_shared_name_prefix_is_not_under_the_root() {
    // `/a/agents-evil` must not match root `/a/agents`: component-wise compare.
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("agents");
    let sibling = base.path().join("agents-evil");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&sibling).unwrap();
    let up = upstream_in(&sibling);
    let allow = vec![root.canonicalize().unwrap()];
    assert!(matches!(
        run(Platform::MacOs, &up, restricted(), &allow),
        Classification::Candidate { .. }
    ));
}

#[test]
fn row6_restricted_macos_outside_allowlist_is_only_a_candidate() {
    let allowed = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let up = upstream_in(outside.path());
    let allow = vec![allowed.path().canonicalize().unwrap()];
    assert_eq!(
        run(Platform::MacOs, &up, restricted(), &allow),
        Classification::Candidate {
            upstream: up.canonicalize().unwrap()
        }
    );
}

#[test]
fn symlink_under_allowlist_classifies_by_its_outside_target() {
    // The link lives under an allowlist root but points outside it. SBPL
    // matches the resolved path, so this is a candidate, not row 5.
    let allowed = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = upstream_in(outside.path());
    let link = allowed.path().join("agent.sock");
    if !make_file_symlink(&target, &link) {
        return;
    }
    let allow = vec![allowed.path().canonicalize().unwrap()];
    assert_eq!(
        run(Platform::MacOs, &link, restricted(), &allow),
        Classification::Candidate {
            upstream: target.canonicalize().unwrap()
        }
    );
}

#[test]
fn symlink_outside_allowlist_pointing_inside_is_audit_only() {
    // The reverse: an innocent-looking path that resolves into the allowlist.
    let allowed = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = upstream_in(allowed.path());
    let link = outside.path().join("agent.sock");
    if !make_file_symlink(&target, &link) {
        return;
    }
    let root = allowed.path().canonicalize().unwrap();
    assert_eq!(
        run(
            Platform::MacOs,
            &link,
            restricted(),
            std::slice::from_ref(&root)
        ),
        audit(Reason::UpstreamInAllowlist(root))
    );
}

#[test]
fn reason_codes_match_the_spec_strings() {
    let codes = [
        (Reason::PlatformNoFsSandbox, "platform_no_fs_sandbox"),
        (
            Reason::PlatformNoUnixSocketControl,
            "platform_no_unix_socket_control",
        ),
        (Reason::UnrestrictedNetwork, "unrestricted_network"),
        (Reason::UpstreamUnresolved, "upstream_unresolved"),
        (
            Reason::UpstreamInAllowlist(PathBuf::from("/x")),
            "upstream_in_allowlist",
        ),
    ];
    for (reason, code) in codes {
        assert_eq!(reason.code(), code);
    }
}

/// Symlinks need privilege on Windows; skip there rather than fail.
#[cfg(unix)]
fn make_file_symlink(target: &Path, link: &Path) -> bool {
    std::os::unix::fs::symlink(target, link).unwrap();
    true
}
#[cfg(unix)]
fn make_dir_symlink(target: &Path, link: &Path) -> bool {
    make_file_symlink(target, link)
}
#[cfg(windows)]
fn make_file_symlink(target: &Path, link: &Path) -> bool {
    std::os::windows::fs::symlink_file(target, link).is_ok()
}
#[cfg(windows)]
fn make_dir_symlink(target: &Path, link: &Path) -> bool {
    std::os::windows::fs::symlink_dir(target, link).is_ok()
}

/// The real allowlist, not a re-listed copy: any socket placed under a root
/// `unix_socket_allow_paths` returns must classify as row 5. Roots this host
/// cannot write to are skipped; at least one (the temp root) must be usable.
#[cfg(target_os = "macos")]
#[test]
fn every_writable_root_of_the_real_allowlist_is_row5() {
    let scratch = tempfile::tempdir().unwrap();
    let policy = crate::sandbox::SandboxPolicy {
        scratch_dir: Some(scratch.path().to_path_buf()),
        ..Default::default()
    };
    let allow = crate::sandbox::macos::unix_socket_allow_paths(&policy);
    let mut checked = 0;
    for root in &allow {
        let Ok(dir) = tempfile::tempdir_in(root) else {
            continue;
        };
        let up = upstream_in(dir.path());
        assert!(
            matches!(
                run(Platform::MacOs, &up, restricted(), &allow),
                Classification::AuditOnly(Reason::UpstreamInAllowlist(_))
            ),
            "socket under allowlist root {} was not classified row 5",
            root.display()
        );
        checked += 1;
    }
    assert!(checked > 0, "no allowlist root was writable: {allow:?}");
}
