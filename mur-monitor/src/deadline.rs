//! Stalled / soft / hard semantics (spec §stalled 與期限). Pure: the caller
//! passes the previous flags and `now`; nothing here reads a clock or a
//! store. Deadlines run from `work_started_at`; the stalled timer runs from
//! `last_progress_at`, which only moves when the progress token CHANGES.

use chrono::{DateTime, Utc};

use crate::spec::Policy;

/// `observed == None` (an `unknown` answer) is neither progress nor a reset.
pub fn advance_progress(
    prev_at: DateTime<Utc>,
    prev_token: Option<&str>,
    observed: Option<&str>,
    now: DateTime<Utc>,
) -> (DateTime<Utc>, Option<String>) {
    match observed {
        Some(t) if Some(t) != prev_token => (now, Some(t.to_string())),
        _ => (prev_at, prev_token.map(str::to_string)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadlineVerdict {
    pub stalled_since: Option<DateTime<Utc>>,
    pub stalled_newly: bool,
    pub recovered: bool,
    pub soft_notified: bool,
    pub soft_newly: bool,
    pub hard_reached: bool,
    pub hard_newly: bool,
}

fn to_chrono(d: std::time::Duration) -> chrono::Duration {
    chrono::Duration::from_std(d).unwrap_or(chrono::Duration::MAX)
}

pub fn evaluate(
    policy: &Policy,
    work_started_at: DateTime<Utc>,
    last_progress_at: DateTime<Utc>,
    prev_stalled_since: Option<DateTime<Utc>>,
    prev_soft: bool,
    prev_hard: bool,
    now: DateTime<Utc>,
) -> DeadlineVerdict {
    let is_stalled = now - last_progress_at >= to_chrono(policy.stalled_after());
    let stalled_since = if is_stalled {
        prev_stalled_since.or(Some(now))
    } else {
        None
    };
    let soft = now - work_started_at >= to_chrono(policy.soft_deadline());
    let hard = now - work_started_at >= to_chrono(policy.hard_deadline());
    DeadlineVerdict {
        stalled_since,
        stalled_newly: is_stalled && prev_stalled_since.is_none(),
        recovered: !is_stalled && prev_stalled_since.is_some(),
        soft_notified: prev_soft || soft,
        soft_newly: soft && !prev_soft,
        hard_reached: prev_hard || hard,
        hard_newly: hard && !prev_hard,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 15, 12, 0, 0).unwrap()
    }

    #[test]
    fn same_token_does_not_advance_progress() {
        let (at, tok) = advance_progress(t0(), Some("a"), Some("a"), t0() + Duration::minutes(5));
        assert_eq!(at, t0());
        assert_eq!(tok.as_deref(), Some("a"));
    }

    #[test]
    fn new_token_advances_progress_and_none_keeps_previous() {
        let now = t0() + Duration::minutes(5);
        let (at, tok) = advance_progress(t0(), Some("a"), Some("b"), now);
        assert_eq!((at, tok.as_deref()), (now, Some("b")));
        let (at, tok) = advance_progress(t0(), Some("a"), None, now);
        assert_eq!(
            (at, tok.as_deref()),
            (t0(), Some("a")),
            "an unknown answer is not progress and not regress"
        );
        let (at, tok) = advance_progress(t0(), None, Some("first"), now);
        assert_eq!((at, tok.as_deref()), (now, Some("first")));
    }

    #[test]
    fn stalled_fires_once_at_twenty_minutes_and_recovers() {
        let p = Policy::default();
        let v = evaluate(
            &p,
            t0(),
            t0(),
            None,
            false,
            false,
            t0() + Duration::minutes(19),
        );
        assert!(v.stalled_since.is_none() && !v.stalled_newly);
        let v = evaluate(
            &p,
            t0(),
            t0(),
            None,
            false,
            false,
            t0() + Duration::minutes(20),
        );
        assert_eq!(v.stalled_since, Some(t0() + Duration::minutes(20)));
        assert!(v.stalled_newly);
        let again = evaluate(
            &p,
            t0(),
            t0(),
            v.stalled_since,
            false,
            false,
            t0() + Duration::minutes(25),
        );
        assert_eq!(
            again.stalled_since, v.stalled_since,
            "keeps the first stall time"
        );
        assert!(!again.stalled_newly, "only the first entry is an event");
        let rec = evaluate(
            &p,
            t0(),
            t0() + Duration::minutes(26),
            again.stalled_since,
            false,
            false,
            t0() + Duration::minutes(26),
        );
        assert!(rec.stalled_since.is_none() && rec.recovered);
    }

    #[test]
    fn soft_and_hard_count_from_work_start_and_fire_once() {
        let p = Policy::default();
        let v = evaluate(
            &p,
            t0(),
            t0() + Duration::hours(3),
            None,
            false,
            false,
            t0() + Duration::hours(3),
        );
        assert!(v.soft_notified && v.soft_newly && !v.hard_reached);
        let v2 = evaluate(
            &p,
            t0(),
            t0() + Duration::hours(4),
            None,
            true,
            false,
            t0() + Duration::hours(4),
        );
        assert!(v2.soft_notified && !v2.soft_newly);
        let h = evaluate(
            &p,
            t0(),
            t0() + Duration::hours(8),
            None,
            true,
            false,
            t0() + Duration::hours(8),
        );
        assert!(h.hard_reached && h.hard_newly);
        let h2 = evaluate(
            &p,
            t0(),
            t0() + Duration::hours(9),
            None,
            true,
            true,
            t0() + Duration::hours(9),
        );
        assert!(h2.hard_reached && !h2.hard_newly);
    }
}
