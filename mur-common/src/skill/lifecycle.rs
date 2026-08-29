//! Pure-function lifecycle + decay layer. Functions take inputs, return
//! outputs, never touch disk. M5b's sweep calls these to decide
//! transitions and persist; M5a's doctor calls them for read-only display.

use chrono::{DateTime, Duration, Utc};

use crate::config::SkillLifecycleConfig;
use crate::skill::stats::{LifecycleState, SkillStats};
use crate::skill::types::Provenance;

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
        LifecycleState::Deprecated | LifecycleState::Archived | LifecycleState::Destroyed => 365.0,
    }
}

// ── Per-kind decay curves (memory federation P1) ─────────────────────────
// One lifecycle, kind-appropriate dynamics: behavioral rules iterate fast
// and must decay fast; environment facts stay true for a long time.

/// Default half-life multiplier for `kind=rule` notes.
pub const NOTE_RULE_HALF_LIFE_FACTOR: f64 = 0.5;
/// Default half-life multiplier for `kind=fact` notes.
pub const NOTE_FACT_HALF_LIFE_FACTOR: f64 = 2.0;

/// The two knowledge shapes a `Category::Note` skill can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteKind {
    /// Behavioral guidance — short half-life, fast iteration.
    Rule,
    /// Semantic statement about the environment — long half-life.
    Fact,
}

impl NoteKind {
    /// Compile-time default decay multiplier for this kind. The lifecycle
    /// sweep applies the config-overridable values from
    /// [`LifecycleThresholds`]; retrieval-side decay uses these defaults so
    /// the two decay systems agree unless deliberately tuned apart.
    pub fn default_half_life_factor(self) -> f64 {
        match self {
            NoteKind::Rule => NOTE_RULE_HALF_LIFE_FACTOR,
            NoteKind::Fact => NOTE_FACT_HALF_LIFE_FACTOR,
        }
    }
}

/// Kind of a note manifest: `Category::Note` + a `rule` tag → `Rule`; a plain
/// note is a `Fact` (facts are the default larval form; rules are explicit).
/// Non-note skills have no kind.
pub fn note_kind(manifest: &crate::skill::SkillManifest) -> Option<NoteKind> {
    if manifest.category != crate::skill::Category::Note {
        return None;
    }
    if manifest.tags.iter().any(|t| t == "rule") {
        Some(NoteKind::Rule)
    } else {
        Some(NoteKind::Fact)
    }
}

/// Decay half-life multiplier for `manifest` under thresholds `t` — notes get
/// per-kind curves; everything else (and a missing manifest) is 1.0.
pub fn half_life_factor_for(
    manifest: Option<&crate::skill::SkillManifest>,
    t: &LifecycleThresholds,
) -> f64 {
    match manifest.and_then(note_kind) {
        Some(NoteKind::Rule) => t.note_rule_half_life_factor,
        Some(NoteKind::Fact) => t.note_fact_half_life_factor,
        None => 1.0,
    }
}

/// Runtime-immutable lifecycle thresholds, derived from `SkillLifecycleConfig`.
///
/// Created once per sweep and threaded through to `next_state`. The `Default`
/// impl mirrors the compile-time constants below so callers that don't have
/// access to config (e.g. the doctor's read-only preview) continue to work
/// without any config file.
#[derive(Debug, Clone)]
pub struct LifecycleThresholds {
    pub promote_draft_uses: u64,
    pub promote_emerging_uses: u64,
    pub promote_emerging_success_rate: f64,
    pub promote_emerging_age_days: i64,
    pub promote_stable_uses: u64,
    pub promote_stable_success_rate: f64,
    pub promote_stable_age_days: i64,
    pub demote_emerging_uses: u64,
    pub demote_emerging_success_rate: f64,
    pub demote_stable_uses: u64,
    pub demote_stable_success_rate: f64,
    pub deprecated_success_rate: f64,
    pub deprecated_no_success_days: i64,
    pub auto_archive_confidence: f64,
    pub auto_archive_age_days: i64,
    /// Per-kind decay multipliers (federation P1). See [`half_life_factor_for`].
    pub note_rule_half_life_factor: f64,
    pub note_fact_half_life_factor: f64,
}

impl Default for LifecycleThresholds {
    fn default() -> Self {
        Self {
            promote_draft_uses: PROMOTE_DRAFT_USES,
            promote_emerging_uses: PROMOTE_EMERGING_USES,
            promote_emerging_success_rate: PROMOTE_EMERGING_SUCCESS_RATE,
            promote_emerging_age_days: PROMOTE_EMERGING_AGE_DAYS,
            promote_stable_uses: PROMOTE_STABLE_USES,
            promote_stable_success_rate: PROMOTE_STABLE_SUCCESS_RATE,
            promote_stable_age_days: PROMOTE_STABLE_AGE_DAYS,
            demote_emerging_uses: DEMOTE_EMERGING_USES,
            demote_emerging_success_rate: DEMOTE_EMERGING_SUCCESS_RATE,
            demote_stable_uses: DEMOTE_STABLE_USES,
            demote_stable_success_rate: DEMOTE_STABLE_SUCCESS_RATE,
            deprecated_success_rate: DEPRECATED_SUCCESS_RATE,
            deprecated_no_success_days: DEPRECATED_NO_SUCCESS_DAYS,
            auto_archive_confidence: AUTO_ARCHIVE_CONFIDENCE,
            auto_archive_age_days: AUTO_ARCHIVE_AGE_DAYS,
            note_rule_half_life_factor: NOTE_RULE_HALF_LIFE_FACTOR,
            note_fact_half_life_factor: NOTE_FACT_HALF_LIFE_FACTOR,
        }
    }
}

impl From<&SkillLifecycleConfig> for LifecycleThresholds {
    fn from(c: &SkillLifecycleConfig) -> Self {
        Self {
            promote_draft_uses: c.promote_draft_uses,
            promote_emerging_uses: c.promote_emerging_uses,
            promote_emerging_success_rate: c.promote_emerging_success_rate,
            promote_emerging_age_days: c.promote_emerging_age_days,
            promote_stable_uses: c.promote_stable_uses,
            promote_stable_success_rate: c.promote_stable_success_rate,
            promote_stable_age_days: c.promote_stable_age_days,
            demote_emerging_uses: c.demote_emerging_uses,
            demote_emerging_success_rate: c.demote_emerging_success_rate,
            demote_stable_uses: c.demote_stable_uses,
            demote_stable_success_rate: c.demote_stable_success_rate,
            deprecated_success_rate: c.deprecated_success_rate,
            deprecated_no_success_days: c.deprecated_no_success_days,
            auto_archive_confidence: c.auto_archive_confidence,
            auto_archive_age_days: c.auto_archive_age_days,
            note_rule_half_life_factor: c.note_rule_half_life_factor,
            note_fact_half_life_factor: c.note_fact_half_life_factor,
        }
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
///
/// Pass `&LifecycleThresholds::default()` when config is not available
/// (e.g. doctor read-only preview).
pub fn next_state(
    stats: &SkillStats,
    now: DateTime<Utc>,
    t: &LifecycleThresholds,
) -> LifecycleState {
    let current = stats.lifecycle_state;

    // Destroyed is terminal — the files are gone; the sweep never calls
    // next_state for destroyed skills, but guard defensively.
    if current == LifecycleState::Destroyed {
        return LifecycleState::Destroyed;
    }

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
            if decayed < t.auto_archive_confidence && age_days > t.auto_archive_age_days {
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
        .map(|ts| (now - ts).num_days())
        .unwrap_or(i64::MAX);

    // Deprecation predicate — applies from any non-Archived state.
    if !stats.pinned
        && current != LifecycleState::Archived
        && (success_rate < t.deprecated_success_rate && stats.usage_count >= 5
            || no_success_days > t.deprecated_no_success_days)
    {
        return LifecycleState::Deprecated;
    }

    // Promotion ladder. Each rung requires the prior rung's criteria.
    let can_canonical = stats.pinned
        && stats.success_count >= t.promote_stable_uses
        && success_rate >= t.promote_stable_success_rate
        && age_days >= t.promote_stable_age_days;
    let can_stable = stats.success_count >= t.promote_emerging_uses
        && success_rate >= t.promote_emerging_success_rate
        && age_days >= t.promote_emerging_age_days;
    let can_emerging = stats.success_count >= t.promote_draft_uses;

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

/// Cap a proposed lifecycle state for LLM-authored, uncurated skills.
///
/// PURE. The promotion ladder (`next_state`) is provenance-blind; this
/// applies the A1 curation gate on top: an `Llm` skill that no human has
/// curated cannot rise above `Emerging`, no matter how good its run stats
/// look. `Human`/`Hybrid` skills, curated skills, and a disabled gate all
/// pass `proposed` through unchanged. States at or below `Emerging` are
/// never raised.
/// Whether decay may demote this item.
///
/// Decay arrived on 2026-02-25 as "Pattern Maturity + Automatic Decay" — the
/// filter that made *automatic mining* survivable, because most of what a miner
/// produces is noise and something had to prune it. The pattern pipeline was
/// removed in #404 and notes inherited the machinery, but not the condition it
/// depended on: a note holds what a human said or wrote, and an explicit
/// statement does not become less true because nothing retrieved it this month.
///
/// So decay prunes exactly the set the promotion gate holds back — machine
/// proposals no human has stood behind yet. `Human` and `Hybrid` are authored
/// or reviewed by a person; a curated `Llm` item has been endorsed. None of
/// them decay.
///
/// This governs demotion only. Evidence of actual failure (the broken-workflow
/// fast path) still demotes anything, because that is a measurement, not a
/// guess about staleness.
pub fn decay_may_demote(provenance: Provenance, curated: bool) -> bool {
    provenance == Provenance::Llm && !curated
}

pub fn cap_for_provenance(
    proposed: LifecycleState,
    provenance: Provenance,
    curated: bool,
    gate_enabled: bool,
) -> LifecycleState {
    let gated = gate_enabled && provenance == Provenance::Llm && !curated;
    if gated && lifecycle_rank(proposed) > lifecycle_rank(LifecycleState::Emerging) {
        LifecycleState::Emerging
    } else {
        proposed
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
    if stats.pinned && lifecycle_rank(to) < lifecycle_rank(from) {
        return false;
    }
    let elapsed = now - stats.lifecycle_changed_at;
    if elapsed < Duration::hours(MIN_DWELL_HOURS) {
        return false;
    }
    true
}

/// Total order over lifecycle states. Public because the federation snapshot
/// floor (mur-core) compares against the same ranking — a duplicated table
/// drifting from this one would silently change what federates.
pub fn lifecycle_rank(s: LifecycleState) -> u8 {
    match s {
        LifecycleState::Destroyed => 0,
        LifecycleState::Archived => 1,
        LifecycleState::Deprecated => 2,
        LifecycleState::Draft => 3,
        LifecycleState::Emerging => 4,
        LifecycleState::Stable => 5,
        LifecycleState::Canonical => 6,
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
        let s1 = next_state(&stats, now, &LifecycleThresholds::default());
        let s2 = next_state(&stats, now, &LifecycleThresholds::default());
        assert_eq!(s1, s2);
    }

    #[test]
    fn promotion_full_ladder() {
        let now = Utc::now();
        // Enough successes, age, and rate to reach Canonical (with pin)
        let stats = make_stats(LifecycleState::Draft, 50, 45, 40, 0, 1.0, true);
        assert_eq!(
            next_state(&stats, now, &LifecycleThresholds::default()),
            LifecycleState::Canonical
        );
    }

    #[test]
    fn emerging_without_pin() {
        let now = Utc::now();
        let stats = make_stats(LifecycleState::Draft, 5, 4, 10, 1, 1.0, false);
        // 5 successes ≥ PROMOTE_DRAFT_USES=3, but not enough age for Stable
        assert_eq!(
            next_state(&stats, now, &LifecycleThresholds::default()),
            LifecycleState::Emerging
        );
    }

    #[test]
    fn deprecation_from_low_success_rate() {
        let now = Utc::now();
        let stats = make_stats(LifecycleState::Emerging, 10, 2, 30, 10, 0.5, false);
        // success_rate = 0.2 < 0.3, usage >= 5
        assert_eq!(
            next_state(&stats, now, &LifecycleThresholds::default()),
            LifecycleState::Deprecated
        );
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
        let state = next_state(&stats, now_fixed, &LifecycleThresholds::default());
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

    #[test]
    fn cap_blocks_llm_uncurated_above_emerging() {
        // Stable proposed, LLM, not curated, gate on → capped to Emerging.
        assert_eq!(
            cap_for_provenance(LifecycleState::Stable, Provenance::Llm, false, true),
            LifecycleState::Emerging
        );
        // Canonical likewise capped.
        assert_eq!(
            cap_for_provenance(LifecycleState::Canonical, Provenance::Llm, false, true),
            LifecycleState::Emerging
        );
    }

    #[test]
    fn cap_is_noop_for_human_curated_or_disabled() {
        // Human authorship → never gated.
        assert_eq!(
            cap_for_provenance(LifecycleState::Stable, Provenance::Human, false, true),
            LifecycleState::Stable
        );
        // LLM but curated → gate open.
        assert_eq!(
            cap_for_provenance(LifecycleState::Stable, Provenance::Llm, true, true),
            LifecycleState::Stable
        );
        // Gate disabled by config → no cap.
        assert_eq!(
            cap_for_provenance(LifecycleState::Canonical, Provenance::Llm, false, false),
            LifecycleState::Canonical
        );
        // At or below Emerging → unchanged even when gated.
        assert_eq!(
            cap_for_provenance(LifecycleState::Draft, Provenance::Llm, false, true),
            LifecycleState::Draft
        );
    }
}

#[cfg(test)]
mod note_kind_tests {
    use super::*;

    fn manifest(category: &str, tags: &str) -> crate::skill::SkillManifest {
        crate::skill::parse_canonical(&format!(
            "name: t\nversion: 1.0.0\npublisher: human:t\ndescription: d\ncategory: {category}\ntags: {tags}\ncontent:\n  abstract: a\n  context: c\n"
        ))
        .unwrap()
    }

    #[test]
    fn note_kind_rule_fact_and_none() {
        assert_eq!(note_kind(&manifest("note", "[rule]")), Some(NoteKind::Rule));
        assert_eq!(note_kind(&manifest("note", "[]")), Some(NoteKind::Fact));
        assert_eq!(note_kind(&manifest("context", "[rule]")), None);
    }

    #[test]
    fn half_life_factor_rule_halves_fact_doubles_skill_unchanged() {
        let t = LifecycleThresholds::default();
        assert!(half_life_factor_for(Some(&manifest("note", "[rule]")), &t) < 1.0);
        assert!(half_life_factor_for(Some(&manifest("note", "[]")), &t) > 1.0);
        assert_eq!(half_life_factor_for(None, &t), 1.0);
        assert_eq!(
            half_life_factor_for(Some(&manifest("context", "[]")), &t),
            1.0
        );
    }
}
