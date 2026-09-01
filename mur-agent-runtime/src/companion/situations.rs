//! Situation × time-of-day weight table.
//!
//! Spec §4.6.

use chrono::{DateTime, Local, NaiveDate, Timelike};
use mur_common::companion::Situation;
use rand::RngCore;
use rand::distributions::{Distribution, WeightedIndex};

/// Returns weights for `(morning_greeting, gentle_check_in, share_quote, share_link, workflow_nudge)`
/// at the given hour. WorkflowNudge is never picked by the proactive rhythm (0.0 weight).
/// `None` means quiet hours (no situation eligible).
fn weights_by_hour(hour: u32) -> Option<[f32; 5]> {
    match hour {
        6..=9 => Some([0.6, 0.0, 0.4, 0.0, 0.0]),
        10..=13 => Some([0.0, 0.4, 0.2, 0.4, 0.0]),
        14..=17 => Some([0.0, 0.5, 0.0, 0.5, 0.0]),
        18..=21 => Some([0.0, 0.0, 0.6, 0.4, 0.0]),
        _ => None, // 22:00–06:00 → quiet
    }
}

/// Pick a situation for a given local time, suppressing `morning_greeting` if
/// already sent today. Returns `None` during quiet hours or if every weight is 0.
pub fn pick_for_hour<R: RngCore>(
    now_local: DateTime<Local>,
    morning_sent_today: Option<NaiveDate>,
    rng: &mut R,
) -> Option<Situation> {
    let mut weights = weights_by_hour(now_local.hour())?;
    let today = now_local.date_naive();
    if morning_sent_today == Some(today) {
        weights[0] = 0.0;
    }
    if weights.iter().all(|w| *w <= 0.0) {
        return None;
    }
    let dist = WeightedIndex::new(weights).ok()?;
    Some(match dist.sample(rng) {
        0 => Situation::MorningGreeting,
        1 => Situation::GentleCheckIn,
        2 => Situation::ShareQuote,
        3 => Situation::ShareLink,
        4 => Situation::WorkflowNudge,
        _ => unreachable!(),
    })
}

#[cfg(test)]
mod scheduled_is_unreachable_tests {
    use super::*;
    use chrono::TimeZone;
    use rand::SeedableRng;

    /// `Situation::Scheduled` is supplied directly by the scheduler and must
    /// never be produced by the hourly picker — the picker answers "say
    /// something? say what?", and a schedule already has both answers.
    ///
    /// This pins the claim in the enum's own doc comment. The guarantee is
    /// structural (`weights_by_hour` returns a fixed 5-wide table and the
    /// sample arms stop at index 4), but a sixth weight added later would
    /// silently make it reachable, and nothing else would notice.
    #[test]
    fn the_hourly_picker_never_yields_scheduled() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(0xB0A7);
        let mut seen = 0usize;
        for hour in 0..24 {
            let now = Local.with_ymd_and_hms(2026, 9, 1, hour, 0, 0).unwrap();
            for _ in 0..200 {
                if let Some(s) = pick_for_hour(now, None, &mut rng) {
                    seen += 1;
                    assert_ne!(
                        s,
                        Situation::Scheduled,
                        "the picker reached Scheduled at hour {hour}"
                    );
                }
            }
        }
        assert!(seen > 0, "the loop must actually have picked something");
    }
}
