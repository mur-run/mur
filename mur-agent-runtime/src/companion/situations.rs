//! Situation × time-of-day weight table.
//!
//! Spec §4.6.

use chrono::{DateTime, Local, NaiveDate, Timelike};
use mur_common::companion::Situation;
use rand::RngCore;
use rand::distributions::{Distribution, WeightedIndex};

/// Returns weights for `(morning_greeting, gentle_check_in, share_quote, share_link)`
/// at the given hour. `None` means quiet hours (no situation eligible).
fn weights_by_hour(hour: u32) -> Option<[f32; 4]> {
    match hour {
        6..=9 => Some([0.6, 0.0, 0.4, 0.0]),
        10..=13 => Some([0.0, 0.4, 0.2, 0.4]),
        14..=17 => Some([0.0, 0.5, 0.0, 0.5]),
        18..=21 => Some([0.0, 0.0, 0.6, 0.4]),
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
        _ => unreachable!(),
    })
}
