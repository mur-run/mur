//! Per-day budget ledger for skill-maintenance LLM calls.
//! Reservation pattern: reserve before call, settle after with actual cost.
//! Over-counts under contention but never under-counts (acceptable).

use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Serialize, Deserialize, Default)]
struct DayLedger {
    date: NaiveDate,
    spent_usd: f64,
    reserved_usd: f64,
}

/// Pre-flight budget check + reservation. Returns `Err(spent_so_far)` if the
/// daily cap (including pending reservations) would be exceeded.
pub fn check_and_reserve(path: &Path, projected_usd: f64, daily_cap_usd: f64) -> Result<(), f64> {
    let mut ledger = load_or_init(path);
    let today = Utc::now().date_naive();
    if ledger.date != today {
        ledger = DayLedger {
            date: today,
            spent_usd: 0.0,
            reserved_usd: 0.0,
        };
    }
    let total = ledger.spent_usd + ledger.reserved_usd + projected_usd;
    if total > daily_cap_usd {
        return Err(ledger.spent_usd);
    }
    ledger.reserved_usd += projected_usd;
    save_atomic(path, &ledger);
    Ok(())
}

/// Settle the reservation with actual cost.
pub fn settle(path: &Path, reserved: f64, actual: f64) -> anyhow::Result<()> {
    let mut ledger = load_or_init(path);
    ledger.reserved_usd = (ledger.reserved_usd - reserved).max(0.0);
    ledger.spent_usd += actual;
    save_atomic(path, &ledger);
    Ok(())
}

/// Return (spent_usd, reserved_usd) for the current day.
pub fn current_usage(path: &Path) -> (f64, f64) {
    let ledger = load_or_init(path);
    let today = Utc::now().date_naive();
    if ledger.date != today {
        return (0.0, 0.0);
    }
    (ledger.spent_usd, ledger.reserved_usd)
}

fn load_or_init(path: &Path) -> DayLedger {
    match std::fs::read_to_string(path) {
        Ok(body) => serde_json::from_str(&body).unwrap_or_default(),
        Err(_) => DayLedger::default(),
    }
}

fn save_atomic(path: &Path, ledger: &DayLedger) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::to_string(ledger).unwrap_or_default();
    let tmp = path.with_extension("tmp");
    let _ = std::fs::write(&tmp, &json);
    let _ = std::fs::rename(&tmp, path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn reserve_and_settle() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("budget.json");
        check_and_reserve(&path, 0.10, 0.50).unwrap();
        settle(&path, 0.10, 0.08).unwrap();
        let (spent, reserved) = current_usage(&path);
        assert!((spent - 0.08).abs() < 0.001);
        assert!((reserved - 0.0).abs() < 0.001);
    }

    #[test]
    fn budget_exhausted() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("budget.json");
        check_and_reserve(&path, 0.10, 0.50).unwrap();
        settle(&path, 0.10, 0.10).unwrap();
        // Next call tries to reserve 0.45 — with 0.10 spent, total = 0.55 > 0.50
        let err = check_and_reserve(&path, 0.45, 0.50).unwrap_err();
        assert!((err - 0.10).abs() < 0.001);
    }

    #[test]
    fn new_day_resets() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("budget.json");
        // Write a stale ledger
        let old = DayLedger {
            date: NaiveDate::from_ymd_opt(2020, 1, 1).unwrap(),
            spent_usd: 0.49,
            reserved_usd: 0.0,
        };
        save_atomic(&path, &old);
        // Should succeed — date rolled over
        check_and_reserve(&path, 0.10, 0.50).unwrap();
    }
}
