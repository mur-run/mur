//! `mur limits <name>` — every execution knob in force for a fleet or an
//! agent, each with the scope it came from (spec 2026-09-12 §3.9), plus the
//! legacy keys that will stop applying at the runtime switch (§6).

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use mur_common::limits::{Limits, ResolvedLimits, Scope, Source, Stuck, resolve};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Fleet(String),
    Agent(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub knob: &'static str,
    pub value: String,
    pub source: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LimitsReport {
    pub target: Target,
    pub rows: Vec<Row>,
    /// Legacy keys still present in the files, each with what happens to it.
    pub stale: Vec<String>,
    pub attended_note: &'static str,
}

/// A name is a fleet if `fleets/<name>/fleet.yaml` loads, else an agent if
/// `agents/<name>/profile.yaml` loads. Both missing is an error that says so
/// — an empty report for a typo would be worse than none.
pub fn detect_target(mur_home: &Path, name: &str) -> Result<Target> {
    if crate::cmd::fleet::store::load_fleet(mur_home, name).is_ok() {
        return Ok(Target::Fleet(name.to_string()));
    }
    if mur_common::agent::AgentProfile::load(mur_home, name).is_ok() {
        return Ok(Target::Agent(name.to_string()));
    }
    Err(anyhow!(
        "`{name}` is neither a fleet (~/.mur/fleets/{name}/fleet.yaml) nor an agent (~/.mur/agents/{name}/profile.yaml)"
    ))
}

const STEP4_NOTE: &str =
    "IGNORED since 2.79 — remove it; the bounds are limits: deadline / stuck / cost_usd";

pub(crate) fn fmt_dur(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s == 0 {
        return "0s".to_string();
    }
    if s.is_multiple_of(3600) {
        format!("{}h", s / 3600)
    } else if s.is_multiple_of(60) {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

fn source_label(src: Source, legacy_deadline: bool, legacy_cost: bool, knob: &str) -> String {
    match (src, knob) {
        (Source::Fleet, "deadline") if legacy_deadline => {
            "fleet.yaml (legacy loop.deadline)".into()
        }
        (Source::Fleet, "cost_usd") if legacy_cost => "fleet.yaml (legacy loop.budget_usd)".into(),
        (s, _) => s.label().to_string(),
    }
}

fn rows_from(
    r: &ResolvedLimits,
    legacy_deadline: bool,
    legacy_cost: bool,
    cost_applies: Option<String>, // None = applies; Some(why) = does not
) -> Vec<Row> {
    let deadline = Row {
        knob: "deadline",
        value: match r.deadline.value {
            Some(d) => fmt_dur(d),
            None => "—".into(),
        },
        source: source_label(r.deadline.source, legacy_deadline, false, "deadline"),
        note: None,
    };
    let stuck = Row {
        knob: "stuck",
        value: match r.stuck.value {
            Stuck::Off => "off".into(),
            Stuck::After(d) => fmt_dur(d),
        },
        source: r.stuck.source.label().to_string(),
        note: None,
    };
    let cost = match cost_applies {
        None => Row {
            knob: "cost_usd",
            value: match r.cost_usd.value {
                Some(c) => format!("${c:.2}"),
                None => "—".into(),
            },
            source: source_label(r.cost_usd.source, false, legacy_cost, "cost_usd"),
            note: None,
        },
        Some(why) => Row {
            knob: "cost_usd",
            value: "—".into(),
            source: String::new(),
            note: Some(why),
        },
    };
    vec![deadline, stuck, cost]
}

pub fn report(mur_home: &Path, target: &Target) -> Result<LimitsReport> {
    let cfg = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml"));
    let global = cfg.limits.clone();
    let none = Limits::default();
    match target {
        Target::Fleet(name) => {
            let fleet = crate::cmd::fleet::store::load_fleet(mur_home, name)?;
            let explicit = fleet.limits.is_some();
            let fl = fleet.limits_or_legacy();
            let legacy_deadline = !explicit && fl.as_ref().is_some_and(|l| l.deadline.is_some());
            let legacy_cost = !explicit && fl.as_ref().is_some_and(|l| l.cost_usd.is_some());
            let resolved = resolve(Scope::FleetRun, &global, fl.as_ref(), None, &none)
                .map_err(|e| anyhow!("{e}"))?;
            let billing = crate::cmd::fleet::billing::fleet_billing(mur_home, &fleet);
            let cost_applies = if billing.billable {
                if billing.unknown.is_empty() {
                    None
                } else {
                    // Applies, but say why: unknown counts as metered.
                    None
                }
            } else {
                Some("runs on local/subscription models — a cost cap does not apply".into())
            };
            let mut rows = rows_from(&resolved, legacy_deadline, legacy_cost, cost_applies);
            if !billing.unknown.is_empty() {
                let who: Vec<String> = billing
                    .unknown
                    .iter()
                    .map(|(a, m)| format!("{a} ({m})"))
                    .collect();
                rows[2].note = Some(format!(
                    "billing unknown for {} — treated as metered; mark a local model with `billing: local` in models.yaml",
                    who.join(", ")
                ));
            }
            let mut stale = Vec::new();
            if let Some(lc) = &fleet.loop_cfg {
                if lc.max_iterations != 0 {
                    stale.push(format!(
                        "loop.max_iterations: {} in fleet.yaml — {STEP4_NOTE}",
                        lc.max_iterations
                    ));
                }
                if explicit && lc.budget_usd > 0.0 {
                    stale.push(format!(
                        "loop.budget_usd: {} in fleet.yaml — shadowed by limits: (remove it)",
                        lc.budget_usd
                    ));
                }
                if explicit && !lc.deadline.trim().is_empty() {
                    stale.push(format!(
                        "loop.deadline: {} in fleet.yaml — shadowed by limits: (remove it)",
                        lc.deadline.trim()
                    ));
                }
            }
            Ok(LimitsReport {
                target: target.clone(),
                rows,
                stale,
                attended_note: "attended runs (murmur) have no hard stops; stuck only warns",
            })
        }
        Target::Agent(name) => {
            let profile = mur_common::agent::AgentProfile::load(mur_home, name)
                .with_context(|| format!("load agent {name}"))?;
            let resolved = resolve(
                Scope::SingleTask,
                &global,
                None,
                profile.limits.as_ref(),
                &none,
            )
            .map_err(|e| anyhow!("{e}"))?;
            let registry =
                mur_common::model::ModelRegistry::load_from(&mur_home.join("models.yaml")).ok();
            let billing = profile
                .model_ref
                .as_ref()
                .and_then(|r| registry.as_ref()?.models.get(r))
                .map(|e| e.billing_or_inferred());
            let cost_applies = match billing {
                Some(mur_common::model::BillingMode::UsageBilled) => None,
                Some(mur_common::model::BillingMode::Local) => {
                    Some("model is local — a cost cap does not apply".into())
                }
                Some(mur_common::model::BillingMode::Subscription) => {
                    Some("model is subscription-billed — a cost cap does not apply".into())
                }
                None => None,
            };
            let mut rows = rows_from(&resolved, false, false, cost_applies);
            if billing.is_none() {
                rows[2].note = Some("billing unknown — treated as metered; mark a local model with `billing: local` in models.yaml".into());
            }
            let mut stale = Vec::new();
            if let Some(n) = profile.hitl.max_iterations {
                stale.push(format!(
                    "hitl.max_iterations: {n} in profile.yaml — {STEP4_NOTE}"
                ));
            }
            if let Some(n) = profile.hitl.max_tokens {
                stale.push(format!(
                    "hitl.max_tokens: {n} in profile.yaml — {STEP4_NOTE}"
                ));
            }
            Ok(LimitsReport {
                target: target.clone(),
                rows,
                stale,
                attended_note: "attended turns (murmur, Hub chat) have no hard stops; stuck only warns",
            })
        }
    }
}

pub fn render_human(r: &LimitsReport) -> String {
    let mut out = String::new();
    match &r.target {
        Target::Fleet(n) => out.push_str(&format!("scope: fleet {n}\n")),
        Target::Agent(n) => out.push_str(&format!("scope: agent {n}\n")),
    }
    for row in &r.rows {
        if row.source.is_empty() {
            out.push_str(&format!(
                "{:<10} {:<8} {}\n",
                row.knob,
                row.value,
                row.note.clone().unwrap_or_default()
            ));
        } else {
            out.push_str(&format!(
                "{:<10} {:<8} ← {}\n",
                row.knob, row.value, row.source
            ));
            if let Some(n) = &row.note {
                out.push_str(&format!("           {n}\n"));
            }
        }
    }
    out.push_str(&format!("note: {}\n", r.attended_note));
    for s in &r.stale {
        out.push_str(&format!("stale: {s}\n"));
    }
    out
}

pub fn render_json(r: &LimitsReport) -> serde_json::Value {
    let (kind, name) = match &r.target {
        Target::Fleet(n) => ("fleet", n),
        Target::Agent(n) => ("agent", n),
    };
    serde_json::json!({
        "target": {"kind": kind, "name": name},
        "rows": r.rows.iter().map(|row| serde_json::json!({
            "knob": row.knob, "value": row.value, "source": row.source, "note": row.note,
        })).collect::<Vec<_>>(),
        "stale": r.stale,
        "attended_note": r.attended_note,
    })
}

pub fn cmd_limits(name: &str, json: bool) -> Result<()> {
    let home = crate::cmd::agent::resolve_mur_home()?;
    let target = detect_target(&home, name)?;
    let r = report(&home, &target)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&render_json(&r))?);
    } else {
        print!("{}", render_human(&r));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::fleet::{Fleet, FleetLoop};

    fn home_with_fleet(fleet: &Fleet) -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        crate::cmd::fleet::store::save_fleet(t.path(), fleet).unwrap();
        t
    }

    fn dev(loop_cfg: Option<FleetLoop>, limits: Option<mur_common::limits::Limits>) -> Fleet {
        Fleet {
            name: "dev".into(),
            display_name: String::new(),
            goal: "g".into(),
            router: None,
            team_id: None,
            members: vec!["pm".into()],
            channel_id: "fleet-dev".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg,
            parallel: None,
            hitl: None,
            requires_programs: vec![],
            limits,
        }
    }

    /// The report says, per knob, the value AND where it came from — the
    /// whole point of the command. With no models.yaml the fleet's billing is
    /// unknown → billable, so cost_usd applies and shows as unset.
    #[test]
    fn a_fleet_report_names_each_source() {
        let f = dev(
            None,
            Some(mur_common::limits::Limits {
                deadline: Some("2h".into()),
                stuck: None,
                cost_usd: None,
            }),
        );
        let t = home_with_fleet(&f);
        let r = report(t.path(), &Target::Fleet("dev".into())).unwrap();
        let by = |k: &str| r.rows.iter().find(|row| row.knob == k).unwrap();
        assert_eq!(by("deadline").value, "2h");
        assert_eq!(by("deadline").source, "fleet.yaml");
        assert_eq!(by("stuck").value, "10m");
        assert_eq!(by("stuck").source, "built-in default");
        assert_eq!(by("cost_usd").value, "—");
        assert!(
            by("cost_usd")
                .note
                .as_deref()
                .unwrap_or("")
                .contains("billing unknown"),
            "{:?}",
            by("cost_usd").note
        );
        assert!(r.stale.is_empty());
        let text = render_human(&r);
        assert!(
            text.contains("deadline") && text.contains("← fleet.yaml"),
            "{text}"
        );
    }

    /// §6: a pre-`limits:` fleet is reported from its legacy fields, labelled
    /// legacy, and its max_iterations is listed as stale with the step-4 note.
    #[test]
    fn a_legacy_fleet_is_read_and_its_stale_keys_named() {
        let f = dev(
            Some(FleetLoop {
                trigger: "manual".into(),
                max_iterations: 8,
                budget_usd: 5.0,
                deadline: "2h".into(),
                done_when: String::new(),
            }),
            None,
        );
        let t = home_with_fleet(&f);
        let r = report(t.path(), &Target::Fleet("dev".into())).unwrap();
        let by = |k: &str| r.rows.iter().find(|row| row.knob == k).unwrap();
        assert_eq!(by("deadline").source, "fleet.yaml (legacy loop.deadline)");
        assert_eq!(by("cost_usd").value, "$5.00");
        assert!(
            r.stale.iter().any(|s| s.contains("loop.max_iterations: 8")),
            "{:?}",
            r.stale
        );
        assert!(
            r.stale[0].contains("IGNORED since 2.79"),
            "says it is ignored: {}",
            r.stale[0]
        );
    }

    /// Unknown name → a clear error, not an empty report.
    #[test]
    fn an_unknown_name_is_an_error_naming_both_kinds() {
        let t = tempfile::tempdir().unwrap();
        let e = detect_target(t.path(), "nobody").unwrap_err().to_string();
        assert!(e.contains("fleet") && e.contains("agent"), "{e}");
    }

    #[test]
    fn json_carries_rows_and_stale_keys() {
        let f = dev(None, None);
        let t = home_with_fleet(&f);
        let r = report(t.path(), &Target::Fleet("dev".into())).unwrap();
        let j = render_json(&r);
        assert_eq!(j["target"]["kind"], "fleet");
        assert_eq!(j["rows"].as_array().unwrap().len(), 3);
        assert!(j["stale"].is_array());
    }
}
