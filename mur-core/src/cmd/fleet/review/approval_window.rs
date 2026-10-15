//! How long the operator has left to answer an `Allow?` prompt (#1762).
//!
//! The runtime denies a gated call when `hitl.timeout_secs` runs out, and an
//! answer given after that is rejected as expired. The prompt used to give no
//! hint of that clock, so a slow answer looked identical to a timely one. The
//! window is one deadline per approval notification (the runtime awaits a
//! whole batch against a single deadline), so it is stamped when the
//! notification arrives and every call in the batch shares it.

use std::time::{Duration, Instant};

/// The deadline an approval notification carries, counted from `now`.
/// `None` when the notification has no usable `timeout_ms` (an older runtime):
/// the prompt then shows no clock rather than a made-up one.
pub(super) fn deadline_from(hitl: &serde_json::Value, now: Instant) -> Option<Instant> {
    let ms = hitl.get("timeout_ms")?.as_u64()?;
    now.checked_add(Duration::from_millis(ms))
}

/// `4m 59s`, `45s`. Rounded up, so the label never claims less time than is
/// left and never reads `0s` while the window is still open.
pub(super) fn remaining_label(left: Duration) -> String {
    let secs = left.as_secs() + u64::from(left.subsec_nanos() > 0);
    match secs {
        0..=59 => format!("{secs}s"),
        _ => format!("{}m {:02}s", secs / 60, secs % 60),
    }
}

/// The clock part of the prompt, evaluated at `now` so a re-ask shows the
/// time left *then*. Empty when there is no deadline.
pub(super) fn prompt_clock(deadline: Option<Instant>, now: Instant) -> String {
    match deadline {
        None => String::new(),
        Some(d) => match d.checked_duration_since(now).filter(|l| !l.is_zero()) {
            Some(left) => format!("({} left) ", remaining_label(left)),
            None => "(window closed — a late answer is rejected) ".to_string(),
        },
    }
}

#[cfg(test)]
#[path = "approval_window_tests.rs"]
mod approval_window_tests;
