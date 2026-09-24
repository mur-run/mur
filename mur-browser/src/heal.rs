//! Offline self-healing for `mur browser replay --heal`.
//!
//! Design: `docs/superpowers/specs/2026-09-24-browser-replay-heal-design.md`.
//! This module currently holds the heal budget (D4); locator matching and
//! verification land in later steps of plan Task 5.

/// Default share of element steps allowed to heal in `mode: test`.
pub const DEFAULT_HEAL_RATIO: f32 = 0.2;

/// Tolerance added before `floor` so `f32` ratios such as `0.7` (stored as
/// `0.69999998…`) still give the exact integer the user wrote, e.g. `10 × 0.7 → 7`.
const RATIO_EPSILON: f64 = 1e-6;

/// How many element steps may heal out of `total` at `max_ratio`.
///
/// `total == 0` allows nothing; otherwise at least one heal is always allowed,
/// so short recordings are not failed by a single stale locator.
pub fn allowed_heals(total: u32, max_ratio: f32) -> u32 {
    if total == 0 {
        return 0;
    }
    let raw = (f64::from(total) * f64::from(max_ratio) + RATIO_EPSILON).floor();
    // `raw` is in 0..=total because callers validate max_ratio to 0.0..=1.0;
    // clamp anyway so a bad ratio can never widen the budget past `total`.
    let floored = raw.clamp(0.0, f64::from(total)) as u32;
    floored.max(1)
}

/// Whether `healed` element steps exceed the budget for `total` at `max_ratio`.
pub fn budget_exceeded(healed: u32, total: u32, max_ratio: f32) -> bool {
    healed > allowed_heals(total, max_ratio)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_within_ratio_is_not_exceeded() {
        assert!(!budget_exceeded(2, 10, 0.2));
    }

    #[test]
    fn budget_over_ratio_is_exceeded() {
        assert!(budget_exceeded(3, 10, 0.2));
    }

    #[test]
    fn budget_short_run_always_allows_one_heal() {
        assert!(!budget_exceeded(1, 3, 0.2));
    }

    #[test]
    fn budget_short_run_second_heal_is_exceeded() {
        assert!(budget_exceeded(2, 3, 0.2));
    }

    #[test]
    fn budget_no_element_steps_is_never_exceeded() {
        assert!(!budget_exceeded(0, 0, 0.2));
        assert_eq!(allowed_heals(0, 0.2), 0);
    }

    #[test]
    fn allowed_heals_is_exact_for_f32_ratios() {
        assert_eq!(allowed_heals(10, 0.2), 2);
        assert_eq!(allowed_heals(10, 0.7), 7);
        assert_eq!(allowed_heals(10, DEFAULT_HEAL_RATIO), 2);
    }

    #[test]
    fn allowed_heals_never_exceeds_total() {
        assert_eq!(allowed_heals(3, 1.0), 3);
        assert_eq!(allowed_heals(3, 5.0), 3);
        assert_eq!(allowed_heals(3, 0.0), 1);
    }
}
