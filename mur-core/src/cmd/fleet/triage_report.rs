//! `mur fleet triage` — the read side of the calibration loop.
//!
//! Without this command the loop is only half closed: the gate writes
//! verdicts and outcomes to `~/.mur/triage-calibration/`, and nothing ever
//! reads them back. A ledger nobody can see is indistinguishable from one
//! that is not being written, and the whole point of the calibration was to
//! stop triage being an oracle that is never checked.
//!
//! # What this refuses to print
//!
//! A percentage computed from nothing. `Calibration::miss_rate` and
//! `shadow_precision` are `Option`, and this command keeps them optional all
//! the way to the terminal: an unmeasured rate renders as `—`, never as
//! `0%`. That distinction is the difference between "triage has never been
//! wrong" and "triage has never been checked", and they must not look the
//! same in a report that a person uses to decide whether to enforce.

use std::path::Path;

use crate::executor::triage_calibration::{Calibration, CalibrationLog};

/// Default window. Long enough that a lightly used install has a sample at
/// all, short enough that a change in triage's behaviour is not averaged
/// against a month of the old one.
pub const DEFAULT_DAYS: u32 = 14;

/// Render a rate that may not exist. The whole reason this is a function is
/// so the em dash for "no data" is written once and cannot drift into a
/// `0%` at one of the call sites.
fn rate(r: Option<f64>) -> String {
    match r {
        None => "—".to_string(),
        Some(v) => format!("{:.0}%", v * 100.0),
    }
}

/// The advice line. This is the only place that says anything about
/// `triage.enforce`, and it will not recommend turning it on without a
/// shadow precision to point at — the config docs make the same promise, and
/// a report that hedges differently would undo them.
pub fn recommendation(c: &Calibration) -> String {
    match c.shadow_precision() {
        None => "Not enough shadow data to judge enforcement. Leave `triage.enforce: false` \
                 and let it keep recording."
            .to_string(),
        Some(p) if p >= 0.7 => format!(
            "Triage was right about {}/{} of the runs it wanted to stop. Enforcing would \
             have saved those and wrongly blocked {}.",
            c.shadow_held_back_overran,
            c.shadow_held_back,
            c.shadow_held_back - c.shadow_held_back_overran
        ),
        Some(_) => format!(
            "Triage wanted to stop {} runs and only {} would have overrun. Enforcing now \
             would block more good work than bad.",
            c.shadow_held_back, c.shadow_held_back_overran
        ),
    }
}

/// Human table. Counts first, rates second, and the caveat attached to the
/// number it qualifies rather than in a footnote nobody reads.
pub fn render(c: &Calibration, days: u32) -> String {
    let mut s = String::new();
    s.push_str(&format!("triage calibration — last {days} days\n\n"));

    if c.paired == 0 && c.unpaired_verdicts == 0 && c.unpaired_outcomes == 0 {
        s.push_str(
            "  No records yet.\n\n  Triage runs only when `triage.enabled: true` in \
             ~/.mur/config.yaml.\n",
        );
        return s;
    }

    s.push_str(&format!(
        "  judged proceeds     {:>5}   overran {:>4}   miss rate {}\n",
        c.proceeded,
        c.proceeded_overran,
        rate(c.miss_rate())
    ));
    s.push_str(&format!(
        "  proceeds by default {:>5}   overran {:>4}   (no judgement was made)\n",
        c.proceeded_by_default, c.default_overran
    ));
    s.push_str(&format!(
        "  shadow hold-backs   {:>5}   overran {:>4}   precision {}\n",
        c.shadow_held_back,
        c.shadow_held_back_overran,
        rate(c.shadow_precision())
    ));
    s.push_str(&format!(
        "  enforced hold-backs {:>5}                  (unscoreable — run never happened)\n",
        c.held_back
    ));
    s.push_str(&format!(
        "\n  paired {}   unpaired verdicts {}   unpaired outcomes {}\n",
        c.paired, c.unpaired_verdicts, c.unpaired_outcomes
    ));
    s.push_str(&format!("\n  {}\n", recommendation(c)));
    s
}

/// JSON for scripts. Rates stay `null` when unmeasured — serialising them as
/// `0.0` would hand a consumer the exact lie the table refuses to tell.
pub fn render_json(c: &Calibration, days: u32) -> serde_json::Value {
    serde_json::json!({
        "days": days,
        "paired": c.paired,
        "proceeded": c.proceeded,
        "proceeded_overran": c.proceeded_overran,
        "proceeded_by_default": c.proceeded_by_default,
        "default_overran": c.default_overran,
        "held_back": c.held_back,
        "shadow_held_back": c.shadow_held_back,
        "shadow_held_back_overran": c.shadow_held_back_overran,
        "unpaired_verdicts": c.unpaired_verdicts,
        "unpaired_outcomes": c.unpaired_outcomes,
        "miss_rate": c.miss_rate(),
        "shadow_precision": c.shadow_precision(),
        "summary": c.summary(),
        "recommendation": recommendation(c),
    })
}

pub fn cmd_fleet_triage(mur_home: &Path, days: u32, json: bool) -> anyhow::Result<()> {
    let dir = crate::executor::triage_gate::calibration_dir(mur_home);
    let c = CalibrationLog::calibration(&dir, days);
    if json {
        println!("{}", serde_json::to_string_pretty(&render_json(&c, days))?);
    } else {
        print!("{}", render(&c, days));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The central rule, at the boundary where it is easiest to break: an
    /// unmeasured rate must not render as a number. A reader seeing `0%`
    /// concludes triage has never been wrong; the truth is nobody looked.
    #[test]
    fn an_unmeasured_rate_is_a_dash_not_zero_percent() {
        assert_eq!(rate(None), "—");
        assert_ne!(rate(None), "0%");
        assert_eq!(rate(Some(0.0)), "0%");
    }

    /// Same rule in the machine-readable path. `null` and `0.0` mean
    /// opposite things to a script that gates on this.
    #[test]
    fn json_keeps_an_unmeasured_rate_null() {
        let c = Calibration::default();
        let j = render_json(&c, 14);
        assert!(j["miss_rate"].is_null(), "got {}", j["miss_rate"]);
        assert!(j["shadow_precision"].is_null());
    }

    /// A measured zero is real information and must survive as a number —
    /// the dash is for absence, not for good news.
    #[test]
    fn a_measured_zero_is_not_hidden_behind_the_dash() {
        let c = Calibration {
            paired: 3,
            proceeded: 3,
            proceeded_overran: 0,
            ..Default::default()
        };
        assert_eq!(rate(c.miss_rate()), "0%");
        assert!(render(&c, 14).contains("miss rate 0%"));
    }

    /// With no shadow sample the report must not suggest enforcing, however
    /// clean the proceeds look. `shadow_precision` is the only evidence that
    /// can justify it.
    #[test]
    fn without_shadow_data_it_never_recommends_enforcing() {
        let c = Calibration {
            paired: 50,
            proceeded: 50,
            proceeded_overran: 0,
            ..Default::default()
        };
        let r = recommendation(&c);
        assert!(r.contains("Leave `triage.enforce: false`"), "got: {r}");
    }

    /// Low precision must read as an argument AGAINST enforcing, not as a
    /// neutral statistic the reader is left to interpret.
    #[test]
    fn poor_precision_argues_against_enforcing() {
        let c = Calibration {
            shadow_held_back: 10,
            shadow_held_back_overran: 2,
            ..Default::default()
        };
        let r = recommendation(&c);
        assert!(r.contains("more good work than bad"), "got: {r}");
    }

    #[test]
    fn strong_precision_reports_both_sides_of_the_trade() {
        let c = Calibration {
            shadow_held_back: 10,
            shadow_held_back_overran: 9,
            ..Default::default()
        };
        let r = recommendation(&c);
        assert!(r.contains("9/10"), "got: {r}");
        assert!(r.contains("wrongly blocked 1"), "got: {r}");
    }

    /// An empty ledger says so plainly and names the switch, instead of
    /// printing a table of zeros that looks like a measurement.
    #[test]
    fn an_empty_ledger_says_so_and_names_the_switch() {
        let out = render(&Calibration::default(), 14);
        assert!(out.contains("No records yet"), "got: {out}");
        assert!(out.contains("triage.enabled"), "got: {out}");
        assert!(!out.contains("miss rate"), "no fake table: {out}");
    }

    /// Enforced hold-backs must stay visibly unscoreable in the report, the
    /// same way they are excluded from the rate.
    #[test]
    fn enforced_hold_backs_are_labelled_unscoreable() {
        let c = Calibration {
            paired: 4,
            held_back: 4,
            ..Default::default()
        };
        let out = render(&c, 14);
        assert!(out.contains("unscoreable"), "got: {out}");
        assert!(out.contains("miss rate —"), "got: {out}");
    }
}
