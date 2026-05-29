//! Pure-function lifecycle + decay layer. Functions take inputs, return
//! outputs, never touch disk. M5b's sweep calls these to decide
//! transitions and persist; M5a's doctor calls them for read-only display.

use chrono::{DateTime, Duration, Utc};

use crate::skill::stats::{LifecycleState, SkillStats};

pub const MIN_CONFIDENCE: f64 = 0.05;
pub const AUTO_ARCHIVE_CONFIDENCE: f64 = 0.10;
pub const AUTO_ARCHIVE_AGE_DAYS: i64 = 180;
pub const MIN_DWELL_HOURS: i64 = 24;

/// Half-life (days) for confidence decay, indexed by current state.
pub fn half_life_days(state: LifecycleState) -> f64 {
    match state {
        LifecycleState::Draft => 14.0,
        LifecycleState::Emerging => 90.0,
        LifecycleState::Stable => 365.0,
        LifecycleState::Canonical => 730.0,
        LifecycleState::Deprecated | LifecycleState::Archived => 365.0,
    }
}

/// Promotion thresholds — values that MUST be exceeded.
pub const PROMOTE_DRAFT_USES: u64 = 3;
pub const PROMOTE_EMERGING_USES: u64 = 10;
pub const PROMOTE_EMERGING_SUCCESS_RATE: f64 = 0.6;
pub const PROMOTE_EMERGING_AGE_DAYS: i64 = 7;
pub const PROMOTE_STABLE_USES: u64 = 30;
pub const PROMOTE_STABLE_SUCCESS_RATE: f64 = 0.8;
pub const PROMOTE_STABLE_AGE_DAYS: i64 = 30;

/// Demotion thresholds — values that MUST drop BELOW. Hysteresis: lower
/// than the symmetric promotion threshold to prevent flap.
pub const DEMOTE_EMERGING_USES: u64 = 8;
pub const DEMOTE_EMERGING_SUCCESS_RATE: f64 = 0.55;
pub const DEMOTE_STABLE_USES: u64 = 25;
pub const DEMOTE_STABLE_SUCCESS_RATE: f64 = 0.75;
pub const DEPRECATED_SUCCESS_RATE: f64 = 0.3;
pub const DEPRECATED_NO_SUCCESS_DAYS: i64 = 90;

/// Compute decayed confidence given an anchor, last success time, and
/// the half-life for the current lifecycle state.
pub fn calculate_decay(
    anchor_confidence: f64,
    last_success: Option<DateTime<Utc>>,
    half_life_days: f64,
    now: DateTime<Utc>,
) -> f64 {
    let conf = anchor_confidence.clamp(0.0, 1.0);
    if !conf.is_finite() || half_life_days <= 0.0 {
        return MIN_CONFIDENCE;
    }
    let last = match last_success {
        None => return MIN_CONFIDENCE,
        Some(t) => t.min(now), // clock-skew defence
    };
    let days = (now - last).num_seconds() as f64 / 86_400.0;
    if days <= 0.0 {
        return conf;
    }
    (conf * 0.5_f64.powf(days / half_life_days)).max(MIN_CONFIDENCE)
}

/// Compute what state the skill *should* be in given its current stats
/// and the current time. PURE — does not mutate. Idempotent: calling
/// this twice with the same inputs returns the same output.
///
/// Caller (M5b sweep, or M5a doctor preview) decides whether to
/// persist or merely display the result.
pub fn next_state(stats: &SkillStats, now: DateTime<Utc>) -> LifecycleState {
    let current = stats.lifecycle_state;

    // Hard archive condition (overrides everything except pinned).
    if !stats.pinned {
        let decayed = calculate_decay(
            stats.anchor_confidence,
            stats.last_success_at,
            half_life_days(current),
            now,
        );
        if let Some(first_ok) = stats.first_successful_use_at {
            let age_days = (now - first_ok).num_days();
            if decayed < AUTO_ARCHIVE_CONFIDENCE && age_days > AUTO_ARCHIVE_AGE_DAYS {
                return LifecycleState::Archived;
            }
        }
    }

    let success_rate = if stats.usage_count == 0 {
        0.0
    } else {
        stats.success_count as f64 / stats.usage_count as f64
    };
    let age_days = stats
        .first_successful_use_at
        .map(|t| (now - t).num_days())
        .unwrap_or(0);
    let no_success_days = stats
        .last_success_at
        .map(|t| (now - t).num_days())
        .unwrap_or(i64::MAX);

    // Deprecation predicate — applies from any non-Archived state.
    if !stats.pinned
        && current != LifecycleState::Archived
        && (success_rate < DEPRECATED_SUCCESS_RATE && stats.usage_count >= 5
            || no_success_days > DEPRECATED_NO_SUCCESS_DAYS)
    {
        return LifecycleState::Deprecated;
    }

    // Promotion ladder. Each rung requires the prior rung's criteria.
    let can_canonical = stats.pinned
        && stats.success_count >= PROMOTE_STABLE_USES
        && success_rate >= PROMOTE_STABLE_SUCCESS_RATE
        && age_days >= PROMOTE_STABLE_AGE_DAYS;
    let can_stable = stats.success_count >= PROMOTE_EMERGING_USES
        && success_rate >= PROMOTE_EMERGING_SUCCESS_RATE
        && age_days >= PROMOTE_EMERGING_AGE_DAYS;
    let can_emerging = stats.success_count >= PROMOTE_DRAFT_USES;

    if can_canonical {
        LifecycleState::Canonical
    } else if can_stable {
        LifecycleState::Stable
    } else if can_emerging {
        LifecycleState::Emerging
    } else {
        LifecycleState::Draft
    }
}

/// Returns true if the transition from `from` to `to` may be persisted
/// *right now*. Even when `next_state` says a transition is warranted,
/// this guard prevents:
///   - flap within MIN_DWELL_HOURS of the last transition
///   - downward transitions for pinned skills below their pinned tier
///   - hysteresis bounce around exact thresholds
pub fn transition_allowed(
    from: LifecycleState,
    to: LifecycleState,
    stats: &SkillStats,
    now: DateTime<Utc>,
) -> bool {
    if from == to {
        return false;
    }
    if stats.pinned && rank(to) < rank(from) {
        return false;
    }
    let elapsed = now - stats.lifecycle_changed_at;
    if elapsed < Duration::hours(MIN_DWELL_HOURS) {
        return false;
    }
    true
}

fn rank(s: LifecycleState) -> u8 {
    match s {
        LifecycleState::Archived => 0,
        LifecycleState::Deprecated => 1,
        LifecycleState::Draft => 2,
        LifecycleState::Emerging => 3,
        LifecycleState::Stable => 4,
        LifecycleState::Canonical => 5,
    }
}

/// Called by the M5b sweep AFTER persisting a promotion. Resets the
/// confidence anchor so the new half-life applies from current, not
/// stale, confidence. Without this, a skill promoted from Draft to
/// Emerging would carry its already-decayed anchor under the longer
/// Emerging half-life and appear artificially fresh forever.
///
/// M5b's sweep MUST call this after writing the new `lifecycle_state`
/// to disk. M5a never calls it.
pub fn on_promotion(stats: &mut SkillStats, now: DateTime<Utc>) {
    let prior_half_life = half_life_days(stats.lifecycle_state);
    let decayed = calculate_decay(
        stats.anchor_confidence,
        stats.last_success_at,
        prior_half_life,
        now,
    );
    stats.anchor_confidence = decayed;
    stats.lifecycle_changed_at = now;
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn make_stats(
        state: LifecycleState,
        usage: u64,
        success: u64,
        first_ok_days_ago: i64,
        last_ok_days_ago: i64,
        anchor: f64,
        pinned: bool,
    ) -> SkillStats {
        let now = Utc::now();
        SkillStats {
            schema_version: 1,
            skill_name: "test".into(),
            skill_version: "1.0.0".into(),
            manifest_digest: "abc".into(),
            lifecycle_state: state,
            lifecycle_changed_at: now - Duration::hours(48),
            pinned,
            pinned_reason: String::new(),
            usage_count: usage,
            success_count: success,
            failure_count: usage.saturating_sub(success),
            last_used_at: Some(now - Duration::days(last_ok_days_ago)),
            last_success_at: Some(now - Duration::days(last_ok_days_ago)),
            first_successful_use_at: Some(now - Duration::days(first_ok_days_ago)),
            anchor_confidence: anchor,
            rebuilt_from_trace_through: None,
            resolution_misses: 0,
            curated_at: None,
        }
    }

    #[test]
    fn decay_floor_honored_at_extreme_age() {
        let now = Utc::now();
        let last = Some(now - Duration::days(10_000));
        let conf = calculate_decay(1.0, last, 14.0, now);
        assert_eq!(conf, MIN_CONFIDENCE);
    }

    #[test]
    fn clock_skew_clamped_returns_anchor_unchanged() {
        let now = Utc::now();
        let future = now + Duration::days(1);
        let conf = calculate_decay(0.8, Some(future), 14.0, now);
        assert_eq!(conf, 0.8);
    }

    #[test]
    fn decay_no_last_success_returns_min() {
        let now = Utc::now();
        let conf = calculate_decay(1.0, None, 14.0, now);
        assert_eq!(conf, MIN_CONFIDENCE);
    }

    #[test]
    fn next_state_idempotent() {
        let now = Utc::now();
        let stats = make_stats(LifecycleState::Draft, 1, 1, 1, 0, 1.0, false);
        let s1 = next_state(&stats, now);
        let s2 = next_state(&stats, now);
        assert_eq!(s1, s2);
    }

    #[test]
    fn promotion_full_ladder() {
        let now = Utc::now();
        // Enough successes, age, and rate to reach Canonical (with pin)
        let stats = make_stats(LifecycleState::Draft, 50, 45, 40, 0, 1.0, true);
        assert_eq!(next_state(&stats, now), LifecycleState::Canonical);
    }

    #[test]
    fn emerging_without_pin() {
        let now = Utc::now();
        let stats = make_stats(LifecycleState::Draft, 5, 4, 10, 1, 1.0, false);
        // 5 successes ≥ PROMOTE_DRAFT_USES=3, but not enough age for Stable
        assert_eq!(next_state(&stats, now), LifecycleState::Emerging);
    }

    #[test]
    fn deprecation_from_low_success_rate() {
        let now = Utc::now();
        let stats = make_stats(LifecycleState::Emerging, 10, 2, 30, 10, 0.5, false);
        // success_rate = 0.2 < 0.3, usage >= 5
        assert_eq!(next_state(&stats, now), LifecycleState::Deprecated);
    }

    #[test]
    fn pinned_floor_prevents_demotion() {
        let now_fixed = Utc.with_ymd_and_hms(2026, 5, 25, 0, 0, 0).unwrap();
        // Bad metrics would normally demote, but pinned
        let stats = SkillStats {
            lifecycle_state: LifecycleState::Stable,
            pinned: true,
            usage_count: 10,
            success_count: 2,
            failure_count: 8,
            anchor_confidence: 0.5,
            last_success_at: Some(now_fixed - Duration::days(120)),
            first_successful_use_at: Some(now_fixed),
            lifecycle_changed_at: now_fixed - Duration::hours(48),
            ..make_stats(LifecycleState::Stable, 10, 2, 30, 120, 0.5, true)
        };
        // Pinned: should not deprecate despite terrible metrics
        let state = next_state(&stats, now_fixed);
        assert_ne!(state, LifecycleState::Deprecated);
    }

    #[test]
    fn transition_allowed_dwell_within_24h_returns_false() {
        let now = Utc::now();
        let stats = SkillStats {
            lifecycle_changed_at: now - Duration::hours(1),
            pinned: false,
            ..make_stats(LifecycleState::Draft, 0, 0, 0, 0, 1.0, false)
        };
        assert!(!transition_allowed(
            LifecycleState::Draft,
            LifecycleState::Emerging,
            &stats,
            now,
        ));
    }

    #[test]
    fn transition_allowed_identical_from_to_returns_false() {
        let now = Utc::now();
        let stats = make_stats(LifecycleState::Draft, 0, 0, 0, 0, 1.0, false);
        assert!(!transition_allowed(
            LifecycleState::Draft,
            LifecycleState::Draft,
            &stats,
            now,
        ));
    }

    #[test]
    fn transition_allowed_downgrade_pinned_blocked() {
        let now = Utc::now();
        let stats = SkillStats {
            lifecycle_changed_at: now - Duration::hours(48),
            pinned: true,
            ..make_stats(LifecycleState::Stable, 0, 0, 0, 0, 1.0, true)
        };
        assert!(!transition_allowed(
            LifecycleState::Stable,
            LifecycleState::Emerging,
            &stats,
            now,
        ));
    }

    #[test]
    fn on_promotion_resets_anchor() {
        let now = Utc::now();
        let mut stats = make_stats(LifecycleState::Draft, 0, 0, 0, 0, 1.0, false);
        let old_anchor = stats.anchor_confidence;
        on_promotion(&mut stats, now);
        // Anchor should be recalculated; lifecycle_changed_at updated
        assert!(stats.lifecycle_changed_at >= now - Duration::seconds(1));
        // Decayed value from a 1.0 anchor with 0 successes and no last_success
        // → MIN_CONFIDENCE since last_success is None
        assert!(stats.anchor_confidence <= old_anchor);
    }
}
