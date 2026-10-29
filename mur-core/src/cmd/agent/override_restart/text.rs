//! The one wording source for override outcomes. The CLI prints these and the
//! Hub shows them verbatim (Hub spec §Failure paths), so the two surfaces
//! can never drift apart.

use super::run::FILE_REMAINS;
use super::{Cleanup, OverrideOutcome, Step};

/// The user-facing sentence for an outcome; the CLI and the Hub both say
/// exactly this. The CLI adds a `✓` in front of an applied one.
pub fn outcome_text(name: &str, outcome: &OverrideOutcome) -> String {
    match outcome {
        OverrideOutcome::Applied { flags, detail, .. } => with_detail(
            format!(
                "'{name}' is running with the override: {}",
                flags.cli_names().join(" ")
            ),
            detail.as_deref(),
        ),
        OverrideOutcome::Rejected { reason, detail, .. } => with_detail(
            format!(
                "'{name}' restarted, but rejected the override: {reason}. It is running with \
                 normal permissions."
            ),
            detail.as_deref(),
        ),
        OverrideOutcome::Failed {
            step,
            cleanup,
            detail,
        } => failed_text(name, step, cleanup.as_ref(), detail),
        OverrideOutcome::Abandoned { reason } => format!("override abandoned: {reason}"),
    }
}

/// A step that stopped the run before anything was published.
pub fn step_text(step: &Step) -> String {
    match step {
        Step::Refusal { reason } => reason.clone(),
        other => format!("{other:?}"),
    }
}

fn with_detail(mut s: String, detail: Option<&str>) -> String {
    if let Some(d) = detail {
        s.push_str(&format!(" (an earlier step reported: {d})"));
    }
    s
}

fn failed_text(name: &str, step: &Step, cleanup: Option<&Cleanup>, detail: &str) -> String {
    let suffix = match cleanup {
        None => return detail.to_owned(),
        Some(Cleanup::Withdrawn) => match step {
            Step::NoAck => format!(
                "the override was withdrawn; '{name}' may still come up, with normal permissions"
            ),
            _ => "the override was withdrawn and nothing is relaxed".to_owned(),
        },
        Some(Cleanup::ConsumedUnacknowledged) => format!(
            "the override was consumed by a boot that has not acknowledged it; it may still come \
             up relaxed. Run `mur agent status {name}`"
        ),
        Some(Cleanup::WithdrawFailed { error }) => format!("{FILE_REMAINS} ({error})"),
    };
    format!("{detail}; {suffix}")
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
