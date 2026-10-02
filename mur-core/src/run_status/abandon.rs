//! The `abandoned` rule: a run whose process died without ever recording a
//! terminal state, long enough ago that nothing will now record one (#1622).
//!
//! Lives here, beside `classify`, so every surface — `mur job status`,
//! `mur fleet status`, `mur job list`, and the `mur_run` monitor adapter —
//! answers with ONE rule. Two copies is how `mur monitor` came to say
//! `abandoned` while `mur fleet status` kept saying `running` for the same
//! dead run.
//!
//! `abandoned` is a reporting verdict, never a stored state and never
//! `failed`: we do not know what the run did, only that it will not say.

use chrono::{DateTime, Utc};

use super::Liveness;

/// How many stale thresholds a DEAD process's last heartbeat must be past
/// before its run counts as `abandoned`. Generous on purpose: `status_of`
/// already reconciles a terminal state from the channel, so this window only
/// has to cover a process that died between its last heartbeat and writing
/// its result — and a false `abandoned` ends a monitor's watch, so err long.
/// Default: 30 s × 30 = 15 min.
pub const ABANDON_AFTER_STALE_MULTIPLE: i32 = 30;

/// The heartbeat age past which a dead, never-settled run is `abandoned`.
/// Derived from the stale threshold, so a user who slows the heartbeat
/// (`runs:` in config.yaml) also widens this window — they cannot drift.
pub fn grace(stale_after: chrono::Duration) -> chrono::Duration {
    stale_after * ABANDON_AFTER_STALE_MULTIPLE
}

/// THE rule. Only `Liveness::Dead` qualifies — a live process with a stale
/// heartbeat (`Stalled`) may yet move, and a rebuilt record with no
/// heartbeat (`Unknown`) has no age to measure. `classify` only reports
/// `Dead` for a non-terminal state, so a finished run never qualifies.
pub fn is_abandoned(
    liveness: Liveness,
    last_heartbeat_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    grace: chrono::Duration,
) -> bool {
    liveness == Liveness::Dead
        && last_heartbeat_at.is_some_and(|beat| now.signed_duration_since(beat) > grace)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g() -> chrono::Duration {
        chrono::Duration::minutes(15)
    }

    #[test]
    fn dead_past_grace_is_abandoned() {
        let now = Utc::now();
        let beat = now - g() - chrono::Duration::seconds(1);
        assert!(is_abandoned(Liveness::Dead, Some(beat), now, g()));
    }

    #[test]
    fn dead_at_or_within_grace_is_not_yet_abandoned() {
        let now = Utc::now();
        for age in [chrono::Duration::seconds(5), g()] {
            assert!(
                !is_abandoned(Liveness::Dead, Some(now - age), now, g()),
                "age {age:?} must still be inside the grace window"
            );
        }
    }

    /// A live process that stopped beating may still finish; a rebuilt
    /// record has no beat to age; a finished run is not a crash.
    #[test]
    fn only_a_dead_process_can_be_abandoned() {
        let now = Utc::now();
        let old = Some(now - g() * 4);
        for l in [
            Liveness::Alive,
            Liveness::Stalled,
            Liveness::Unknown,
            Liveness::NotApplicable,
        ] {
            assert!(!is_abandoned(l, old, now, g()), "{l:?} must not qualify");
        }
        assert!(
            !is_abandoned(Liveness::Dead, None, now, g()),
            "no heartbeat means no age to measure"
        );
    }

    #[test]
    fn grace_scales_with_the_stale_threshold() {
        let stale = chrono::Duration::seconds(30);
        assert_eq!(grace(stale), chrono::Duration::minutes(15));
        assert_eq!(grace(stale * 2), chrono::Duration::minutes(30));
    }
}
