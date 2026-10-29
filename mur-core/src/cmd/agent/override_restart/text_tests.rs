use mur_common::agent::OverrideFlags;

use super::*;

fn flags() -> OverrideFlags {
    OverrideFlags {
        no_hitl: true,
        full_perms: true,
        ..OverrideFlags::default()
    }
}

fn failed(step: Step, cleanup: Option<Cleanup>) -> String {
    outcome_text(
        "a",
        &OverrideOutcome::Failed {
            step,
            cleanup,
            detail: "D".into(),
        },
    )
}

#[test]
fn applied_names_the_flags_and_folds_in_the_earlier_step() {
    let plain = OverrideOutcome::Applied {
        flags: flags(),
        late: false,
        detail: None,
    };
    let text = outcome_text("a", &plain);
    assert!(
        text.starts_with("'a' is running with the override: "),
        "{text}"
    );
    assert!(!text.contains("earlier step"), "{text}");

    let late = OverrideOutcome::Applied {
        flags: flags(),
        late: true,
        detail: Some("could not stop 'a'".into()),
    };
    assert!(outcome_text("a", &late).ends_with(" (an earlier step reported: could not stop 'a')"));
}

#[test]
fn rejected_names_the_reason_and_the_detail() {
    let text = outcome_text(
        "a",
        &OverrideOutcome::Rejected {
            reason: "override rejected: stale".into(),
            late: false,
            detail: Some("X".into()),
        },
    );
    assert_eq!(
        text,
        "'a' restarted, but rejected the override: override rejected: stale. It is running with \
         normal permissions. (an earlier step reported: X)"
    );
}

#[test]
fn failure_wording_follows_the_cleanup() {
    assert_eq!(failed(Step::Sweep, None), "D");
    assert_eq!(
        failed(Step::Stop, Some(Cleanup::Withdrawn)),
        "D; the override was withdrawn and nothing is relaxed"
    );
    assert_eq!(
        failed(Step::NoAck, Some(Cleanup::Withdrawn)),
        "D; the override was withdrawn; 'a' may still come up, with normal permissions"
    );
    assert!(
        failed(Step::NoAck, Some(Cleanup::ConsumedUnacknowledged))
            .ends_with("Run `mur agent status a`")
    );
    assert_eq!(
        failed(
            Step::Start,
            Some(Cleanup::WithdrawFailed {
                error: "EACCES".into()
            })
        ),
        format!("D; {FILE_REMAINS} (EACCES)")
    );
}

#[test]
fn abandoned_and_refusal() {
    assert_eq!(
        outcome_text(
            "a",
            &OverrideOutcome::Abandoned {
                reason: "MUR Hub quit".into()
            }
        ),
        "override abandoned: MUR Hub quit"
    );
    assert_eq!(step_text(&Step::Refusal { reason: "R".into() }), "R");
    assert_eq!(step_text(&Step::Sweep), "Sweep");
}
