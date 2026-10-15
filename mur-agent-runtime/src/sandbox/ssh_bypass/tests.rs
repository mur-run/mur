use super::*;
use std::path::PathBuf;

/// A real directory tree, canonicalised so `starts_with` sees the same
/// spelling the classifier produces (macOS temp dirs sit behind `/var` ->
/// `/private/var`).
struct Tree {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Tree {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        Self { _tmp: tmp, root }
    }

    /// Create `rel` as an empty file (stand-in for the socket; canonicalize
    /// only needs the path to exist) and return its path.
    fn file(&self, rel: &str) -> PathBuf {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"").unwrap();
        p
    }

    fn dir(&self, rel: &str) -> PathBuf {
        let p = self.root.join(rel);
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}

const RESTRICTED: Option<&[u16]> = Some(&[]);

fn inputs<'a>(
    platform: Platform,
    upstream: &'a Path,
    allow_roots: &'a [PathBuf],
    allow_patterns: &'a [&'a str],
) -> Inputs<'a> {
    Inputs {
        platform,
        net_allow_ports: RESTRICTED,
        upstream,
        allow_roots,
        allow_patterns,
    }
}

fn audit(reason: AuditReason) -> Classification {
    Classification::AuditOnly(reason)
}

#[test]
fn row1_windows_is_audit_only_even_when_everything_else_says_boundary() {
    let t = Tree::new();
    let up = t.file("outside/agent.sock");
    let got = classify(inputs(Platform::Windows, &up, &[], &[]));
    assert_eq!(got, audit(AuditReason::PlatformNoFsSandbox));
}

#[test]
fn a_platform_without_a_backend_is_classified_like_windows() {
    let t = Tree::new();
    let up = t.file("outside/agent.sock");
    let got = classify(inputs(Platform::Other, &up, &[], &[]));
    assert_eq!(got, audit(AuditReason::PlatformNoFsSandbox));
}

#[test]
fn row2_linux_is_audit_only_even_when_everything_else_says_boundary() {
    let t = Tree::new();
    let up = t.file("outside/agent.sock");
    let got = classify(inputs(Platform::Linux, &up, &[], &[]));
    assert_eq!(got, audit(AuditReason::PlatformNoUnixSocketControl));
}

#[test]
fn row3_unrestricted_network_is_audit_only() {
    let t = Tree::new();
    let up = t.file("outside/agent.sock");
    let mut i = inputs(Platform::MacOs, &up, &[], &[]);
    i.net_allow_ports = None;
    assert_eq!(classify(i), audit(AuditReason::UnrestrictedNetwork));
}

/// `Some(ports)` is restricted however many ports it opens: TCP ports never
/// admit a unix-socket connect.
#[test]
fn row3_restricted_with_open_tcp_ports_is_not_unrestricted() {
    let t = Tree::new();
    let up = t.file("outside/agent.sock");
    let mut i = inputs(Platform::MacOs, &up, &[], &[]);
    i.net_allow_ports = Some(&[443, 22]);
    assert!(matches!(classify(i), Classification::Candidate { .. }));
}

#[test]
fn row4_missing_upstream_is_audit_only() {
    let t = Tree::new();
    let up = t.root.join("does-not-exist.sock");
    let got = classify(inputs(Platform::MacOs, &up, &[], &[]));
    assert_eq!(got, audit(AuditReason::UpstreamUnresolved));
}

#[test]
fn row5_upstream_under_an_allow_root_names_that_root() {
    let t = Tree::new();
    let other = t.dir("other-root");
    let allowed = t.dir("allowed");
    let up = t.file("allowed/deep/agent.sock");
    let roots = [other, allowed.clone()];
    let got = classify(inputs(Platform::MacOs, &up, &roots, &[]));
    assert_eq!(
        got,
        audit(AuditReason::UpstreamInAllowlist { root: allowed })
    );
}

/// `starts_with` is per component: a sibling sharing a name prefix
/// (`allowed-not`) is not under `allowed`.
#[test]
fn row5_a_name_prefix_sibling_is_not_under_the_root() {
    let t = Tree::new();
    let allowed = t.dir("allowed");
    let up = t.file("allowed-not/agent.sock");
    let roots = [allowed];
    let got = classify(inputs(Platform::MacOs, &up, &roots, &[]));
    assert!(matches!(got, Classification::Candidate { .. }), "{got:?}");
}

#[test]
fn an_upstream_matching_an_allow_pattern_is_audit_only() {
    let t = Tree::new();
    let up = t.file("mur-short.sock");
    let pattern = format!("^{}$", regex::escape(&up.to_string_lossy()));
    let patterns = [pattern.as_str()];
    let got = classify(inputs(Platform::MacOs, &up, &[], &patterns));
    assert_eq!(got, audit(AuditReason::UpstreamMatchesAllowPattern));
}

/// A pattern that does not compile cannot be ruled out, so it must not let
/// the classification reach "candidate".
#[test]
fn an_unparseable_allow_pattern_fails_closed() {
    let t = Tree::new();
    let up = t.file("outside/agent.sock");
    let patterns = ["(unclosed"];
    let got = classify(inputs(Platform::MacOs, &up, &[], &patterns));
    assert_eq!(got, audit(AuditReason::UpstreamMatchesAllowPattern));
}

#[test]
fn row6_macos_restricted_outside_every_rule_is_only_a_candidate() {
    let t = Tree::new();
    let allowed = t.dir("allowed");
    let up = t.file("outside/agent.sock");
    let roots = [allowed];
    let patterns = ["^/never/matches$"];
    let got = classify(inputs(Platform::MacOs, &up, &roots, &patterns));
    assert_eq!(
        got,
        Classification::Candidate {
            canonical_upstream: up
        }
    );
}

/// The kernel matches the resolved path, so a link placed under an allow
/// root that points outside it classifies by the target.
#[cfg(unix)]
#[test]
fn a_symlink_under_an_allow_root_classifies_by_its_outside_target() {
    let t = Tree::new();
    let allowed = t.dir("allowed");
    let target = t.file("outside/agent.sock");
    let link = allowed.join("agent.sock");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let roots = [allowed];
    let got = classify(inputs(Platform::MacOs, &link, &roots, &[]));
    assert_eq!(
        got,
        Classification::Candidate {
            canonical_upstream: target
        }
    );
}

/// The dangerous direction: a link outside every root must not hide a target
/// that the sandbox allows.
#[cfg(unix)]
#[test]
fn a_symlink_outside_pointing_into_an_allow_root_is_audit_only() {
    let t = Tree::new();
    let allowed = t.dir("allowed");
    let target = t.file("allowed/agent.sock");
    let link = t.dir("outside").join("agent.sock");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let roots = [allowed.clone()];
    let got = classify(inputs(Platform::MacOs, &link, &roots, &[]));
    assert_eq!(
        got,
        audit(AuditReason::UpstreamInAllowlist { root: allowed })
    );
}

#[test]
fn reason_codes_match_the_design_table() {
    let cases = [
        (AuditReason::PlatformNoFsSandbox, "platform_no_fs_sandbox"),
        (
            AuditReason::PlatformNoUnixSocketControl,
            "platform_no_unix_socket_control",
        ),
        (AuditReason::UnrestrictedNetwork, "unrestricted_network"),
        (AuditReason::UpstreamUnresolved, "upstream_unresolved"),
        (
            AuditReason::UpstreamInAllowlist {
                root: PathBuf::from("/x"),
            },
            "upstream_in_allowlist",
        ),
    ];
    for (reason, code) in cases {
        assert_eq!(reason.as_str(), code);
    }
}

/// Wiring against the real profile rules: an upstream under the agents tree
/// the restricted profile allows is audit-only, naming a root taken from
/// `unix_socket_allow_rules` itself rather than a hard-coded path.
#[cfg(target_os = "macos")]
#[test]
fn macos_real_allow_rules_catch_an_upstream_under_an_allowed_root() {
    let t = Tree::new();
    let scratch = t.dir("scratch");
    let policy = crate::sandbox::SandboxPolicy {
        scratch_dir: Some(scratch.clone()),
        net_allow_ports: Some(vec![]),
        ..Default::default()
    };
    let (roots, patterns) = crate::sandbox::macos::unix_socket_allow_rules(&policy);
    assert!(roots.contains(&scratch), "{roots:?}");
    let up = t.file("scratch/ssh-agent.sock");
    let got = classify(inputs(Platform::MacOs, &up, &roots, patterns));
    assert_eq!(
        got,
        audit(AuditReason::UpstreamInAllowlist { root: scratch })
    );
}
