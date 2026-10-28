use super::*;

fn names(n: &[&str]) -> Vec<String> {
    n.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn restart_with_an_override_takes_exactly_one_name() {
    assert_eq!(
        single_target(&names(&["a"]), false, false, false).unwrap(),
        "a"
    );
    for (n, all, stale, dry, what) in [
        (names(&[]), false, false, false, "none"),
        (names(&["a", "b"]), false, false, false, "several"),
        (names(&["a"]), true, false, false, "--all"),
        (names(&["a"]), false, true, false, "--stale"),
        (names(&["a"]), false, false, true, "--dry-run"),
    ] {
        let e = single_target(&n, all, stale, dry).unwrap_err().to_string();
        assert!(e.contains(what) && e.contains("nothing was changed"), "{e}");
    }
}

fn flags() -> OverrideFlags {
    OverrideFlags {
        no_hitl: true,
        full_perms: true,
        ..OverrideFlags::default()
    }
}

#[test]
fn applied_exits_zero_and_late_still_succeeds() {
    report(
        "a",
        &OverrideOutcome::Applied {
            flags: flags(),
            late: true,
            detail: Some("could not stop 'a'".into()),
        },
    )
    .unwrap();
}

#[test]
fn rejected_names_the_reason_and_fails() {
    let e = report(
        "a",
        &OverrideOutcome::Rejected {
            reason: "override rejected: stale".into(),
            late: false,
            detail: None,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(
        e.contains("override rejected: stale") && e.contains("normal permissions"),
        "{e}"
    );
}

#[test]
fn failure_suffixes_follow_the_cleanup() {
    let msg = |step: Step, cleanup: Option<Cleanup>| {
        report(
            "a",
            &OverrideOutcome::Failed {
                step,
                cleanup,
                detail: "D".into(),
            },
        )
        .unwrap_err()
        .to_string()
    };
    assert!(msg(Step::Stop, Some(Cleanup::Withdrawn)).ends_with("nothing is relaxed"));
    assert!(msg(Step::NoAck, Some(Cleanup::Withdrawn)).contains("may still come up"));
    assert!(msg(Step::NoAck, Some(Cleanup::ConsumedUnacknowledged)).contains("mur agent status a"));
    let f = msg(
        Step::Start,
        Some(Cleanup::WithdrawFailed {
            error: "EACCES".into(),
        }),
    );
    assert!(
        f.contains("pending override file remains") && f.contains("EACCES"),
        "{f}"
    );
    assert_eq!(msg(Step::Sweep, None), "D");
}
