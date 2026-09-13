//! `mur limits <name>` — every execution knob in force for a fleet or an
//! agent, each with the scope it came from (spec 2026-09-12 §3.9), plus the
//! legacy keys that will stop applying at the runtime switch (§6).

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use mur_common::limits::{Limits, ResolvedLimits, Scope, Source, Stuck, resolve};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    // The CLI's `--global` flag bypasses `Target` entirely (`dispatch.rs`
    // calls `upsert_global_limits`/`render_limits_block` directly); this
    // variant is constructed only by mur-hub-gui's global-scope LimitsPanel
    // (Hub 3b, spec §10) and by this module's own tests.
    #[allow(dead_code)]
    Global,
    Fleet(String),
    Agent(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub knob: &'static str,
    pub value: String,
    pub source: String,
    pub note: Option<String>,
    /// False only for `cost_usd` on a scope that cannot spend — the only
    /// case where a row is text, not an editable value (spec §10.1, D4).
    pub applies: bool,
    /// The queried scope itself set this value (its own `limits:` carries
    /// the key) — never inherited from a wider scope or the built-in
    /// default. Drives the Hub's dimmed/solid, Override/Reset choice.
    pub local: bool,
    /// The string a user would edit: the scope's OWN value for this key
    /// (not the resolved value), so overriding an inherited row starts
    /// blank and resetting a local row round-trips its literal text.
    pub raw: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LimitsReport {
    pub target: Target,
    pub rows: Vec<Row>,
    /// Legacy keys still present in the files, each with what happens to it.
    pub stale: Vec<Stale>,
    pub attended_note: &'static str,
    /// Can this scope spend at all? Fleet: `FleetBilling.billable`. Agent:
    /// `UsageBilled` or unresolvable (metered by the same conservative rule
    /// billing.rs uses). Global: always true — some model under it might
    /// meter, and the row must not claim otherwise before it knows.
    pub billable: bool,
    /// Only an agent's profile.yaml edit needs a restart to take effect;
    /// fleet.yaml and config.yaml are read fresh on the next run.
    pub needs_restart: bool,
}

/// One legacy key found in the files, structured so a caller can offer
/// "Remove" without re-parsing `render_human`'s prose (spec §10.4).
#[derive(Debug, Clone, PartialEq)]
pub struct Stale {
    pub key: String,
    pub file: String,
    pub value: String,
    pub message: String,
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
    own: &Limits,
    legacy_deadline: bool,
    legacy_cost: bool,
    cost_applies: Option<String>, // None = applies; Some(why) = does not
) -> Vec<Row> {
    // `own` is the queried scope's own (explicit-or-legacy-derived) block.
    // Because report() never sets a narrower scope than the one it queries
    // (no per-call flag participates here), a key present in `own` is
    // exactly the key that wins resolution at this scope — so `local` and
    // `raw` can read `own` directly rather than re-deriving from `r`.
    let deadline = Row {
        knob: "deadline",
        value: match r.deadline.value {
            Some(d) => fmt_dur(d),
            None => "—".into(),
        },
        source: source_label(r.deadline.source, legacy_deadline, false, "deadline"),
        note: None,
        applies: true,
        local: own.deadline.is_some(),
        raw: own.deadline.clone(),
    };
    let stuck = Row {
        knob: "stuck",
        value: match r.stuck.value {
            Stuck::Off => "off".into(),
            Stuck::After(d) => fmt_dur(d),
        },
        source: r.stuck.source.label().to_string(),
        note: None,
        applies: true,
        local: own.stuck.is_some(),
        raw: own.stuck.clone(),
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
            applies: true,
            local: own.cost_usd.is_some(),
            raw: own.cost_usd.map(|c| c.to_string()),
        },
        Some(why) => Row {
            knob: "cost_usd",
            value: "—".into(),
            source: String::new(),
            note: Some(why),
            applies: false,
            local: false,
            raw: None,
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
            let own = fl.clone().unwrap_or_default();
            let mut rows = rows_from(&resolved, &own, legacy_deadline, legacy_cost, cost_applies);
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
                    stale.push(Stale {
                        key: "loop.max_iterations".into(),
                        file: "fleet.yaml".into(),
                        value: lc.max_iterations.to_string(),
                        message: format!(
                            "loop.max_iterations: {} in fleet.yaml — {STEP4_NOTE}",
                            lc.max_iterations
                        ),
                    });
                }
                if explicit && lc.budget_usd > 0.0 {
                    stale.push(Stale {
                        key: "loop.budget_usd".into(),
                        file: "fleet.yaml".into(),
                        value: lc.budget_usd.to_string(),
                        message: format!(
                            "loop.budget_usd: {} in fleet.yaml — shadowed by limits: (remove it)",
                            lc.budget_usd
                        ),
                    });
                }
                if explicit && !lc.deadline.trim().is_empty() {
                    stale.push(Stale {
                        key: "loop.deadline".into(),
                        file: "fleet.yaml".into(),
                        value: lc.deadline.trim().to_string(),
                        message: format!(
                            "loop.deadline: {} in fleet.yaml — shadowed by limits: (remove it)",
                            lc.deadline.trim()
                        ),
                    });
                }
            }
            Ok(LimitsReport {
                target: target.clone(),
                rows,
                stale,
                attended_note: "attended runs (murmur) have no hard stops; stuck only warns",
                billable: billing.billable,
                needs_restart: false,
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
            let own = profile.limits.clone().unwrap_or_default();
            let mut rows = rows_from(&resolved, &own, false, false, cost_applies);
            if billing.is_none() {
                rows[2].note = Some("billing unknown — treated as metered; mark a local model with `billing: local` in models.yaml".into());
            }
            let mut stale = Vec::new();
            if let Some(n) = profile.hitl.max_iterations {
                stale.push(Stale {
                    key: "hitl.max_iterations".into(),
                    file: "profile.yaml".into(),
                    value: n.to_string(),
                    message: format!("hitl.max_iterations: {n} in profile.yaml — {STEP4_NOTE}"),
                });
            }
            if let Some(n) = profile.hitl.max_tokens {
                stale.push(Stale {
                    key: "hitl.max_tokens".into(),
                    file: "profile.yaml".into(),
                    value: n.to_string(),
                    message: format!("hitl.max_tokens: {n} in profile.yaml — {STEP4_NOTE}"),
                });
            }
            // §5's conservative rule, applied here too: UsageBilled or
            // unresolvable can spend; Local/Subscription cannot.
            let billable = !matches!(
                billing,
                Some(mur_common::model::BillingMode::Local)
                    | Some(mur_common::model::BillingMode::Subscription)
            );
            Ok(LimitsReport {
                target: target.clone(),
                rows,
                stale,
                attended_note: "attended turns (murmur, Hub chat) have no hard stops; stuck only warns",
                billable,
                needs_restart: true,
            })
        }
        Target::Global => {
            let resolved =
                resolve(Scope::FleetRun, &global, None, None, &none).map_err(|e| anyhow!("{e}"))?;
            let rows = rows_from(&resolved, &global, false, false, None);
            Ok(LimitsReport {
                target: target.clone(),
                rows,
                stale: Vec::new(),
                attended_note: "attended runs (murmur) have no hard stops; stuck only warns",
                billable: true,
                needs_restart: false,
            })
        }
    }
}

pub fn render_human(r: &LimitsReport) -> String {
    let mut out = String::new();
    match &r.target {
        Target::Global => out.push_str("scope: global\n"),
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
        out.push_str(&format!("stale: {}\n", s.message));
    }
    out
}

pub fn render_json(r: &LimitsReport) -> serde_json::Value {
    let (kind, name): (&str, Option<&str>) = match &r.target {
        Target::Global => ("global", None),
        Target::Fleet(n) => ("fleet", Some(n.as_str())),
        Target::Agent(n) => ("agent", Some(n.as_str())),
    };
    serde_json::json!({
        "target": {"kind": kind, "name": name},
        "rows": r.rows.iter().map(|row| serde_json::json!({
            "knob": row.knob, "value": row.value, "source": row.source, "note": row.note,
            "applies": row.applies, "local": row.local, "raw": row.raw,
        })).collect::<Vec<_>>(),
        "stale": r.stale.iter().map(|s| serde_json::json!({
            "key": s.key, "file": s.file, "value": s.value, "message": s.message,
        })).collect::<Vec<_>>(),
        "attended_note": r.attended_note,
        "billable": r.billable,
        "needs_restart": r.needs_restart,
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
            needs: vec![],
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
            r.stale
                .iter()
                .any(|s| s.message.contains("loop.max_iterations: 8")),
            "{:?}",
            r.stale
        );
        assert_eq!(r.stale[0].key, "loop.max_iterations");
        assert_eq!(r.stale[0].file, "fleet.yaml");
        assert_eq!(r.stale[0].value, "8");
        assert!(
            r.stale[0].message.contains("IGNORED since 2.79"),
            "says it is ignored: {}",
            r.stale[0].message
        );
    }

    /// A full, loadable profile — the shipped agent-creation template with
    /// the name and model_ref swapped in. `AgentProfile` has many required
    /// (non-`#[serde(default)]`) fields; a hand-trimmed YAML silently fails
    /// to parse and `fleet_billing`'s `.ok()` turns that into "unknown",
    /// which would make every billing assertion pass for the wrong reason.
    ///
    /// Line-by-line, not a raw `\n`-anchored substring replace: a Windows git
    /// checkout gives this resource CRLF line endings, so `"name: mur\n"`
    /// never matches and the substitution silently no-ops (Windows CI only).
    fn agent_profile_yaml(name: &str, model_ref: &str) -> String {
        let tmpl = std::fs::read_to_string(
            env!("CARGO_MANIFEST_DIR").to_string()
                + "/../mur-hub-gui/src-tauri/resources/mur-agent-template/profile.yaml",
        )
        .expect("shipped agent template");
        let mut replaced = false;
        let out = tmpl
            .lines()
            .map(|line| {
                if !replaced && line == "name: mur" {
                    replaced = true;
                    format!("name: {name}\nmodel_ref: {model_ref}")
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(replaced, "template's `name: mur` line not found");
        out + "\n"
    }

    fn write_agent(home: &std::path::Path, name: &str, model_ref: &str) {
        let dir = home.join("agents").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("profile.yaml"),
            agent_profile_yaml(name, model_ref),
        )
        .unwrap();
    }

    /// `schema_version` has no `#[serde(default)]` on `ModelRegistry`; a
    /// models.yaml missing it fails to parse, and `fleet_billing`'s
    /// `ModelRegistry::load_from(..).ok()` silently turns that into "no
    /// registry" — every model then reads as unknown-billing (metered),
    /// which would make a "local model" test pass whether or not the
    /// billing plumbing actually worked.
    fn write_local_model_registry(home: &std::path::Path, model_ref: &str, billing: &str) {
        std::fs::write(
            home.join("models.yaml"),
            format!("schema_version: 1\nmodels:\n  {model_ref}:\n    provider: openai\n    model: q\n    billing: {billing}\n"),
        )
        .unwrap();
    }

    /// The fields a panel needs and the CLI never printed: is the row
    /// editable here, did THIS scope set it, and what string does the user
    /// edit. A fleet that set deadline has local=true and raw="2h"; the
    /// stuck row it inherited has local=false and raw=None; cost on a
    /// fleet whose router AND member both resolve to a local model has
    /// applies=false.
    #[test]
    fn rows_say_local_applies_and_raw() {
        let f = dev(
            None,
            Some(mur_common::limits::Limits {
                deadline: Some("2h".into()),
                stuck: None,
                cost_usd: None,
            }),
        );
        let t = home_with_fleet(&f);
        write_local_model_registry(t.path(), "local", "local");
        write_agent(t.path(), "pm", "local");
        write_agent(t.path(), "mur", "local"); // the fleet's router_or_concierge()
        let r = report(t.path(), &Target::Fleet("dev".into())).unwrap();
        let by = |k: &str| r.rows.iter().find(|row| row.knob == k).unwrap();
        assert!(
            by("deadline").local && by("deadline").raw.as_deref() == Some("2h"),
            "{:?}",
            by("deadline")
        );
        assert!(
            !by("stuck").local && by("stuck").raw.is_none(),
            "{:?}",
            by("stuck")
        );
        assert!(!by("cost_usd").applies, "{:?}", by("cost_usd"));
        assert!(!r.billable, "both router and member are local");
        let j = render_json(&r);
        assert_eq!(j["rows"][0]["local"], true);
        assert_eq!(j["billable"], false);
        assert_eq!(j["needs_restart"], false);
    }

    /// Stale keys are structured so a panel can offer Remove per key.
    #[test]
    fn stale_keys_are_structured() {
        let f = dev(
            Some(FleetLoop {
                trigger: "manual".into(),
                max_iterations: 8,
                budget_usd: 0.0,
                deadline: String::new(),
                done_when: String::new(),
            }),
            None,
        );
        let t = home_with_fleet(&f);
        let r = report(t.path(), &Target::Fleet("dev".into())).unwrap();
        assert_eq!(r.stale[0].key, "loop.max_iterations");
        assert_eq!(r.stale[0].file, "fleet.yaml");
        assert_eq!(r.stale[0].value, "8");
        assert!(r.stale[0].message.contains("IGNORED since 2.79"));
        let j = render_json(&r);
        assert_eq!(j["stale"][0]["key"], "loop.max_iterations");
    }

    /// The global scope reports config.yaml's own values: what it set is
    /// local there, the rest is built-in; nothing is inherited from above.
    #[test]
    fn global_target_reports_config_yaml() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("config.yaml"), "limits:\n  stuck: 20m\n").unwrap();
        let r = report(t.path(), &Target::Global).unwrap();
        let by = |k: &str| r.rows.iter().find(|row| row.knob == k).unwrap();
        assert_eq!(by("stuck").source, "~/.mur/config.yaml");
        assert!(by("stuck").local);
        assert_eq!(by("deadline").source, "built-in default");
        assert!(!by("deadline").local);
        assert!(
            by("cost_usd").applies,
            "global cost cap applies to whatever metered model runs under it"
        );
        assert_eq!(render_json(&r)["target"]["kind"], "global");
        assert_eq!(render_json(&r)["target"]["name"], serde_json::Value::Null);
    }

    #[test]
    fn agent_target_needs_restart() {
        let t = tempfile::tempdir().unwrap();
        let mut yaml = agent_profile_yaml("pm", "local");
        yaml.push_str("hitl:\n  max_iterations: 800\n");
        std::fs::create_dir_all(t.path().join("agents/pm")).unwrap();
        std::fs::write(t.path().join("agents/pm/profile.yaml"), yaml).unwrap();
        let r = report(t.path(), &Target::Agent("pm".into())).unwrap();
        assert!(r.needs_restart);
        assert_eq!(r.stale[0].key, "hitl.max_iterations");
        assert_eq!(r.stale[0].value, "800");
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
