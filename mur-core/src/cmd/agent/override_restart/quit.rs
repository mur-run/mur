//! The Hub quit path's claim (Hub spec §Hub quit): one withdraw attempt for
//! an override a job published, with no writer lock, no retry and no wait
//! for an acknowledgement. The job thread then sees `begin_cleanup == false`
//! and touches nothing, so exactly one side audits this nonce.

use std::path::Path;

use super::outcome::{OverrideOutcome, Step};
use super::run::Job;

/// Why the quit path withdrew, for the audit line and the outcome detail.
const QUIT_REASON: &str = "the Hub quit before the override restart finished";

/// One claim `rename`, at most one read of the withdrawn copy and of the
/// lock file, one `unlink`, one audit append. A boot that already consumed
/// the file is classified, not undone.
pub fn withdraw_on_quit(home: &Path, name: &str, nonce: &str, window: &str) -> OverrideOutcome {
    Job::new(home, name, Some(window.to_owned())).withdraw_and_classify(
        nonce,
        Step::Cancelled,
        QUIT_REASON.to_owned(),
    )
}

#[cfg(test)]
#[path = "quit_tests.rs"]
mod tests;
