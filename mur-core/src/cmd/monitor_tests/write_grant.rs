//! `mur monitor add`'s check on the write grant (F2, whole-branch review).
//! A separate file rather than more of `monitor_tests.rs`, which is already
//! within a few dozen lines of CLAUDE.md's 800-line cap; the fixtures
//! (`home`, `go`, `t0`) arrive through `use super::*` the same way
//! `drain_actions/tests/approval.rs` reaches its own.

use super::*;

/// An env var this test requires to be absent. Named, not invented inline,
/// so the assertion below is about resolution failing — not about a name
/// that happens to be free today.
const MISSING_VAR: &str = "MUR_TEST_MONITOR_WRITE_GRANT_DEFINITELY_NOT_SET_K3W";

/// Writes a spec whose single `on_failure` action is `rerun` — the real
/// shape, since that verb is why a write grant exists at all — carrying
/// `grant` verbatim as `source.write_credential_ref`. `mur_run` as the
/// source keeps `add`'s live probe local: the grant check must be reachable
/// without a network call.
fn spec_with_write_grant(d: &Path, grant: &str) -> PathBuf {
    let p = d.join("write-grant.yaml");
    std::fs::write(
        &p,
        format!(
            "schema_version: 1\nname: t\n\
             source: {{ type: mur_run, reference: run-1, write_credential_ref: {grant} }}\n\
             actions:\n  on_failure:\n    - type: rerun\n\
             idempotency_key: k\ncreated_by: {{ actor: user:test }}\n"
        ),
    )
    .unwrap();
    p
}

fn add_with_grant(d: &Path, grant: &str) -> String {
    go(
        d,
        MonitorAction::Add {
            file: spec_with_write_grant(d, grant),
            started_at: None,
        },
    )
    .unwrap()
}

/// The failure this closes: `add` probed the READ credential live and only
/// format-checked the write grant, so a typo (`keychain:mur/githb-write`)
/// passed, the monitor watched CI for hours, parked an approval — and the
/// resolution failure surfaced only AFTER a human approved the rerun. That
/// is the sequence `a_spec_with_rerun_and_no_write_grant_is_refused`'s own
/// comment calls worse than a clear refusal.
///
/// Warning, not refusal, and the monitor is still created: resolution
/// answers `None` for a locked keychain and a headless box as well as for
/// a typo, so refusing here would reject valid specs for something the
/// user cannot fix at that moment. Both halves are asserted, because a
/// `bail!` would also satisfy the first one on its own.
#[test]
fn add_warns_when_the_write_grant_does_not_resolve() {
    assert!(
        std::env::var_os(MISSING_VAR).is_none(),
        "the fixture's premise is that this variable is unset"
    );
    let (d, _envg) = home();
    let out = add_with_grant(d.path(), &format!("env:{MISSING_VAR}"));
    assert!(
        out.contains("write_credential_ref") && out.contains("does not resolve"),
        "the user must learn at add time, not after approving: {out}"
    );
    assert_eq!(
        MonitorStore::open(d.path())
            .unwrap()
            .list(&ListFilter::default())
            .unwrap()
            .len(),
        1,
        "an unresolvable grant is a warning, not a refusal: the monitor is created"
    );
}

/// The negative control, and the reason the test above is not green on a
/// build that warns whenever the field is merely present: same spec, same
/// action, a grant that really resolves. A `file:` ref at mode 0600 is
/// resolvable with no env var and no keychain prompt.
#[test]
fn add_says_nothing_about_a_write_grant_that_resolves() {
    let (d, _envg) = home();
    let secret = d.path().join("gh-write.token");
    std::fs::write(&secret, "ghp_not_a_real_token\n").unwrap();
    // `resolve_file` refuses a group/world-readable secret — on unix only,
    // where a default-umask file would be 0644 and this test would then be
    // green for the wrong reason (unresolvable, like the test above).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let out = add_with_grant(d.path(), &format!("file:{}", secret.display()));
    assert!(
        !out.contains("write_credential_ref"),
        "a grant that resolves is not worth a word: {out}"
    );
    // And the resolved value itself never reaches the user's terminal.
    assert!(!out.contains("ghp_not_a_real_token"), "{out}");
}
