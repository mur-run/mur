//! `mur agent doctor`: bash calls the agent's bash tier ceiling turned into
//! an approval (#1814).
//!
//! On a scheduled agent an unattended approval DEFERS — it never fails — so
//! nothing on screen says the agent stopped. This list is how an operator
//! sees it: what asked, how often, and the tier to raise
//! `bypass_builtin_tier_floor` to if running those unattended is intended.

use std::collections::BTreeMap;
use std::path::Path;

use mur_common::hitl::RiskTier;
use mur_common::telemetry::{METHOD_BASH_CEILING_EXCEEDED, MUR_EVENT_TYPE};

/// Days of telemetry read. Telemetry is one file per UTC day.
pub const LOOKBACK_DAYS: i64 = 7;
/// Example commands shown per tier.
const EXAMPLES_PER_TIER: usize = 2;
/// Example command length on a doctor line.
const EXAMPLE_CHARS: usize = 80;

#[derive(Debug, Default, PartialEq)]
pub struct TierCount {
    pub count: usize,
    pub examples: Vec<String>,
}

/// Over-ceiling calls in `agent_dir/telemetry`, by tier, for the `days`
/// files ending at `today` (`YYYY-MM-DD`). Unreadable files or lines are
/// skipped — doctor never fails over telemetry.
pub fn scan(
    agent_dir: &Path,
    today: chrono::NaiveDate,
    days: i64,
) -> BTreeMap<RiskTier, TierCount> {
    let mut out: BTreeMap<RiskTier, TierCount> = BTreeMap::new();
    for back in 0..days {
        let day = today - chrono::Duration::days(back);
        let path = agent_dir
            .join("telemetry")
            .join(format!("{}.jsonl", day.format("%Y-%m-%d")));
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            if !line.contains(METHOD_BASH_CEILING_EXCEEDED) {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if v[MUR_EVENT_TYPE] != METHOD_BASH_CEILING_EXCEEDED {
                continue;
            }
            let Ok(tier) = serde_json::from_value::<RiskTier>(v["tier"].clone()) else {
                continue;
            };
            let e = out.entry(tier).or_default();
            e.count += 1;
            if e.examples.len() < EXAMPLES_PER_TIER
                && let Some(cmd) = v["command"].as_str()
            {
                e.examples.push(cmd.chars().take(EXAMPLE_CHARS).collect());
            }
        }
    }
    out
}

/// Doctor lines for one agent; empty when nothing went over.
pub fn lines(name: &str, scan: &BTreeMap<RiskTier, TierCount>) -> Vec<String> {
    if scan.is_empty() {
        return Vec::new();
    }
    let tier = |t: RiskTier| {
        serde_json::to_value(t)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| format!("{t:?}"))
    };
    let total: usize = scan.values().map(|c| c.count).sum();
    let mut out = vec![format!(
        "  {name}: {total} bash call(s) over the bash tier ceiling ASKED in the last {LOOKBACK_DAYS} days \
         (#1814); an unattended run parks on each, it does not fail"
    )];
    for (t, c) in scan.iter().rev() {
        out.push(format!(
            "    {:>4} × {}: {}",
            c.count,
            tier(*t),
            c.examples.join(" | ")
        ));
    }
    out.push(
        "    to keep these unattended: set bypass_builtin_tier_floor: <tier> on the exact `bash` rule \
         (privileged always asks)"
            .to_string(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(tier: &str, cmd: &str) -> String {
        serde_json::json!({
            MUR_EVENT_TYPE: METHOD_BASH_CEILING_EXCEEDED,
            "tier": tier,
            "command": cmd,
            "enforced": true,
        })
        .to_string()
    }

    #[test]
    fn counts_by_tier_within_the_window_only() {
        let d = tempfile::tempdir().unwrap();
        let t = d.path().join("telemetry");
        std::fs::create_dir_all(&t).unwrap();
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 13).unwrap();
        std::fs::write(
            t.join("2026-10-13.jsonl"),
            [
                rec("network-egress", "git push origin main"),
                rec("network-egress", "gh pr view 1"),
                rec("destructive", "rm -rf target"),
                // Same method name inside an unrelated payload: not a record.
                serde_json::json!({"note": METHOD_BASH_CEILING_EXCEEDED}).to_string(),
                "not json".into(),
            ]
            .join("\n"),
        )
        .unwrap();
        std::fs::write(t.join("2026-10-07.jsonl"), rec("privileged", "sudo ls")).unwrap();
        // Outside a 7-day window ending 10-13.
        std::fs::write(t.join("2026-10-06.jsonl"), rec("privileged", "sudo old")).unwrap();

        let s = scan(d.path(), today, LOOKBACK_DAYS);
        assert_eq!(s[&RiskTier::NetworkEgress].count, 2);
        assert_eq!(s[&RiskTier::Destructive].count, 1);
        assert_eq!(s[&RiskTier::Privileged].count, 1);
        assert_eq!(
            s[&RiskTier::Privileged].examples,
            vec!["sudo ls".to_string()]
        );
    }

    #[test]
    fn lines_lead_with_the_total_and_highest_tier_first() {
        let mut s = BTreeMap::new();
        s.insert(
            RiskTier::NetworkEgress,
            TierCount {
                count: 3,
                examples: vec!["git push".into()],
            },
        );
        s.insert(
            RiskTier::Privileged,
            TierCount {
                count: 1,
                examples: vec!["sudo ls".into()],
            },
        );
        let l = lines("mur", &s);
        assert!(l[0].contains("mur: 4 bash call(s)"), "{}", l[0]);
        assert!(l[0].contains("ASKED"), "{}", l[0]);
        assert!(l[1].contains("privileged"), "{}", l[1]);
        assert!(l[2].contains("network-egress"), "{}", l[2]);
        assert!(lines("mur", &BTreeMap::new()).is_empty());
    }
}
