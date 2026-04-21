//! Window-label parsing and boundary math for Phase 3.2 rollups.
//!
//! An ISO week label is `"YYYY-Wnn"` (`nn` 2-digit, 1..=53). A month label is
//! `"YYYY-MM"`. Both parse into `chrono::NaiveDate` boundaries for the
//! rollup pipeline's ts-range filter.

use anyhow::{Context, Result};
use chrono::{Datelike, Duration, NaiveDate, Weekday};

/// Parse an ISO-week label like "2026-W16" into (Monday, Sunday) NaiveDates.
pub fn iso_week_bounds(label: &str) -> Result<(NaiveDate, NaiveDate)> {
    let (year_s, week_s) = label
        .split_once("-W")
        .with_context(|| format!("not an ISO week label: {label}"))?;
    let year: i32 = year_s
        .parse()
        .with_context(|| format!("invalid year in {label}"))?;
    if week_s.len() != 2 {
        anyhow::bail!("week number must be zero-padded two digits: {label}");
    }
    let week: u32 = week_s
        .parse()
        .with_context(|| format!("invalid week number in {label}"))?;
    if !(1..=53).contains(&week) {
        anyhow::bail!("week number out of range 1..=53: {label}");
    }
    let monday = NaiveDate::from_isoywd_opt(year, week, Weekday::Mon)
        .with_context(|| format!("no such ISO week: {label}"))?;
    let sunday = monday + Duration::days(6);
    Ok((monday, sunday))
}

/// Monday of the given ISO week label.
pub fn iso_week_monday(label: &str) -> Result<NaiveDate> {
    iso_week_bounds(label).map(|(m, _)| m)
}

/// Parse a month label like "2026-04" into the first day of that month.
pub fn month_first_day(label: &str) -> Result<NaiveDate> {
    let (y, m) = label
        .split_once('-')
        .with_context(|| format!("not a YYYY-MM label: {label}"))?;
    if m.len() != 2 {
        anyhow::bail!("month must be zero-padded two digits: {label}");
    }
    let year: i32 = y
        .parse()
        .with_context(|| format!("invalid year in {label}"))?;
    let month: u32 = m
        .parse()
        .with_context(|| format!("invalid month in {label}"))?;
    NaiveDate::from_ymd_opt(year, month, 1).with_context(|| format!("invalid month: {label}"))
}

/// Return the ISO week label ("YYYY-Wnn") containing `date`.
pub fn iso_week_label_for(date: NaiveDate) -> String {
    let iw = date.iso_week();
    format!("{:04}-W{:02}", iw.year(), iw.week())
}

/// Return the month label ("YYYY-MM") of `date`.
pub fn month_label_for(date: NaiveDate) -> String {
    format!("{:04}-{:02}", date.year(), date.month())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_week_bounds_common_week() {
        // 2026-W16 = Mon 2026-04-13 through Sun 2026-04-19
        let (mon, sun) = iso_week_bounds("2026-W16").unwrap();
        assert_eq!(mon, NaiveDate::from_ymd_opt(2026, 4, 13).unwrap());
        assert_eq!(sun, NaiveDate::from_ymd_opt(2026, 4, 19).unwrap());
    }

    #[test]
    fn iso_week_bounds_year_start() {
        // ISO 2026-W01 = Mon 2025-12-29 through Sun 2026-01-04
        let (mon, sun) = iso_week_bounds("2026-W01").unwrap();
        assert_eq!(mon, NaiveDate::from_ymd_opt(2025, 12, 29).unwrap());
        assert_eq!(sun, NaiveDate::from_ymd_opt(2026, 1, 4).unwrap());
    }

    #[test]
    fn iso_week_bounds_53_week_year() {
        // 2020 had ISO W53. 2020-W53 = Mon 2020-12-28 through Sun 2021-01-03.
        let (mon, sun) = iso_week_bounds("2020-W53").unwrap();
        assert_eq!(mon, NaiveDate::from_ymd_opt(2020, 12, 28).unwrap());
        assert_eq!(sun, NaiveDate::from_ymd_opt(2021, 1, 3).unwrap());
    }

    #[test]
    fn iso_week_bounds_rejects_invalid() {
        assert!(iso_week_bounds("2026-16").is_err()); // missing W
        assert!(iso_week_bounds("2026-W54").is_err()); // out of range
        assert!(iso_week_bounds("2026-W00").is_err()); // out of range
        assert!(iso_week_bounds("2026-Wxx").is_err()); // non-numeric
        assert!(iso_week_bounds("bogus").is_err());
    }

    #[test]
    fn iso_week_monday_matches_bounds() {
        let (mon, _) = iso_week_bounds("2026-W16").unwrap();
        assert_eq!(iso_week_monday("2026-W16").unwrap(), mon);
    }

    #[test]
    fn month_first_day_happy_path() {
        assert_eq!(
            month_first_day("2026-04").unwrap(),
            NaiveDate::from_ymd_opt(2026, 4, 1).unwrap()
        );
        assert_eq!(
            month_first_day("2026-12").unwrap(),
            NaiveDate::from_ymd_opt(2026, 12, 1).unwrap()
        );
    }

    #[test]
    fn month_first_day_rejects_invalid() {
        assert!(month_first_day("2026-13").is_err());
        assert!(month_first_day("2026-00").is_err());
        assert!(month_first_day("2026-4").is_err()); // need zero-pad
        assert!(month_first_day("bogus").is_err());
    }

    #[test]
    fn iso_week_label_for_date() {
        // 2026-04-13 is a Monday in W16
        let d = NaiveDate::from_ymd_opt(2026, 4, 13).unwrap();
        assert_eq!(iso_week_label_for(d), "2026-W16");
        // 2026-04-19 (Sunday of same week)
        let d = NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();
        assert_eq!(iso_week_label_for(d), "2026-W16");
    }

    #[test]
    fn month_label_for_date() {
        assert_eq!(
            month_label_for(NaiveDate::from_ymd_opt(2026, 4, 15).unwrap()),
            "2026-04"
        );
        assert_eq!(
            month_label_for(NaiveDate::from_ymd_opt(2026, 12, 1).unwrap()),
            "2026-12"
        );
    }
}
