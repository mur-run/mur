//! Deterministic-interval scheduler. Decides when a proactive send is eligible.
//! Spec §4.7.

use chrono::{DateTime, Datelike, Local, NaiveTime, TimeZone, Timelike};
use mur_common::agent::QuietHours;

/// Result of a should_send_now check, including the desired interval for telemetry.
pub struct ScheduleDecision {
    pub should_send: bool,
    pub desired_interval_minutes: i64,
    pub minutes_since_last_send: Option<i64>,
}

/// Decide whether to send now per Spec §4.7.
///
/// `active_window_end_today` is the local-time end of the active window today
/// (e.g. 22:00 if quiet_hours starts at 22:00). Caller computes this from
/// `ActiveHours` or `QuietHours`.
///
/// `jitter_minutes` should be sampled from 0..=10 by the caller (per tick, not per send).
pub fn should_send_now(
    now: DateTime<Local>,
    last_send_at: Option<DateTime<Local>>,
    active_window_end_today: DateTime<Local>,
    daily_cap: u8,
    sent_today: u8,
    jitter_minutes: u8,
) -> ScheduleDecision {
    let minutes_since_last_send = last_send_at.map(|t| (now - t).num_minutes());

    let remaining_active_minutes = (active_window_end_today - now).num_minutes().max(0);
    let budget_remaining = (daily_cap as i64 - sent_today as i64).max(1);
    let desired_interval_minutes = remaining_active_minutes / budget_remaining;

    // Hard gates: daily cap reached, or past window end.
    if sent_today >= daily_cap {
        return ScheduleDecision {
            should_send: false,
            desired_interval_minutes,
            minutes_since_last_send,
        };
    }
    if now >= active_window_end_today {
        return ScheduleDecision {
            should_send: false,
            desired_interval_minutes,
            minutes_since_last_send,
        };
    }

    let threshold = desired_interval_minutes - jitter_minutes as i64;
    let should_send = match minutes_since_last_send {
        Some(elapsed) => elapsed >= threshold,
        None => true, // first send: fire immediately
    };

    ScheduleDecision {
        should_send,
        desired_interval_minutes,
        minutes_since_last_send,
    }
}

/// Parse "HH:MM" → NaiveTime. Returns None on invalid format.
pub fn parse_hhmm(s: &str) -> Option<NaiveTime> {
    let (h, m) = s.split_once(':')?;
    let h: u32 = h.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    NaiveTime::from_hms_opt(h, m, 0)
}

/// Compute today's "end of active window" given QuietHours.
/// If quiet_hours.start is e.g. "22:00", the window ends at 22:00 today.
/// If `quiet_hours` is None, returns 23:59:59 today.
pub fn active_window_end_for_today(
    today_local: DateTime<Local>,
    quiet_hours: Option<&QuietHours>,
) -> Option<DateTime<Local>> {
    let date = today_local.date_naive();
    match quiet_hours {
        Some(qh) => {
            let t = parse_hhmm(&qh.start)?;
            Local
                .with_ymd_and_hms(date.year(), date.month(), date.day(), t.hour(), t.minute(), 0)
                .single()
        }
        None => Local
            .with_ymd_and_hms(date.year(), date.month(), date.day(), 23, 59, 59)
            .single(),
    }
}

