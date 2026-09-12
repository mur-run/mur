//! The Hub's limits surface (spec 2026-09-12 §10): three commands over the
//! same resolver `mur limits` uses. Nothing here decides a default or an
//! applicability rule — the report does, and this file only reshapes it.

use std::path::Path;

use mur_core::cmd::limits::{LimitsReport, Target, report};
use mur_core::cmd::limits_write::{
    Patch, remove_stale_key, upsert_global_limits, write_agent_limits, write_fleet_limits,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Clone)]
pub struct LimitsRowView {
    pub knob: String,
    pub value: String,
    pub source: String,
    pub note: Option<String>,
    pub applies: bool,
    pub local: bool,
    pub raw: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct StaleView {
    pub key: String,
    pub file: String,
    pub value: String,
    pub message: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct LimitsView {
    pub scope: String, // "global" | "fleet" | "agent"
    pub name: Option<String>,
    pub rows: Vec<LimitsRowView>,
    pub stale: Vec<StaleView>,
    pub billable: bool,
    pub needs_restart: bool,
    pub attended_note: String,
    pub bounded: String, // "bounded" | "deadline_only" | "unbounded"
    /// Set when the `limits:` block does not resolve (an unparsable value):
    /// `rows` is empty and `bounded` is "unbounded" — a broken block never
    /// auto-runs on a default it did not ask for (spec §5).
    pub error: Option<String>,
}

#[derive(Deserialize)]
pub struct LimitsPatchArg {
    pub deadline: Option<String>,
    pub stuck: Option<String>,
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub unset: Vec<String>,
}

fn target_of(scope: &str, name: Option<&str>) -> Result<Target, String> {
    match (scope, name) {
        ("global", _) => Ok(Target::Global),
        ("fleet", Some(n)) => Ok(Target::Fleet(n.to_string())),
        ("agent", Some(n)) => Ok(Target::Agent(n.to_string())),
        _ => Err(format!("limits: scope `{scope}` needs a name")),
    }
}

/// §5 as a word: the resolver's deadline bounds every scope; a billable one
/// without a cost cap is bounded by the deadline ONLY (amber); a report that
/// failed to resolve is unbounded.
pub(crate) fn bounded_of(r: &LimitsReport) -> &'static str {
    let cost = r.rows.iter().find(|row| row.knob == "cost_usd");
    match cost {
        Some(c) if r.billable && c.applies && c.value == "—" => "deadline_only",
        _ => "bounded",
    }
}

fn view_of(scope: &str, name: Option<&str>, r: LimitsReport) -> LimitsView {
    let bounded = bounded_of(&r).to_string();
    LimitsView {
        scope: scope.to_string(),
        name: name.map(str::to_string),
        rows: r
            .rows
            .iter()
            .map(|row| LimitsRowView {
                knob: row.knob.to_string(),
                value: row.value.clone(),
                source: row.source.clone(),
                note: row.note.clone(),
                applies: row.applies,
                local: row.local,
                raw: row.raw.clone(),
            })
            .collect(),
        stale: r
            .stale
            .iter()
            .map(|s| StaleView {
                key: s.key.clone(),
                file: s.file.clone(),
                value: s.value.clone(),
                message: s.message.clone(),
            })
            .collect(),
        billable: r.billable,
        needs_restart: r.needs_restart,
        attended_note: r.attended_note.to_string(),
        bounded,
        error: None,
    }
}

fn error_view(scope: &str, name: Option<&str>, e: String) -> LimitsView {
    LimitsView {
        scope: scope.to_string(),
        name: name.map(str::to_string),
        rows: vec![],
        stale: vec![],
        billable: true,
        needs_restart: false,
        attended_note: String::new(),
        bounded: "unbounded".into(),
        error: Some(e),
    }
}

/// The testable core of `limits_resolve` — no Tauri, no `mur_home_path()`.
pub(crate) fn resolve_in(home: &Path, scope: &str, name: Option<&str>) -> LimitsView {
    match target_of(scope, name).and_then(|t| report(home, &t).map_err(|e| format!("{e:#}"))) {
        Ok(r) => view_of(scope, name, r),
        Err(e) => error_view(scope, name, e),
    }
}

/// The testable core of `limits_set`: writes exactly one scope, then
/// resolves it fresh so the caller always renders what is actually on disk.
pub(crate) fn set_in(
    home: &Path,
    scope: &str,
    name: Option<&str>,
    patch: LimitsPatchArg,
) -> Result<LimitsView, String> {
    let p = Patch {
        deadline: patch.deadline,
        stuck: patch.stuck,
        cost_usd: patch.cost_usd,
        unset: patch.unset,
    };
    match target_of(scope, name)? {
        Target::Global => upsert_global_limits(&home.join("config.yaml"), &p),
        Target::Fleet(n) => write_fleet_limits(home, &n, &p),
        Target::Agent(n) => write_agent_limits(&n, &p),
    }
    .map_err(|e| format!("{e:#}"))?;
    Ok(resolve_in(home, scope, name))
}

#[tauri::command]
pub fn limits_resolve(scope: String, name: Option<String>) -> Result<LimitsView, String> {
    Ok(resolve_in(&crate::mur_home_path(), &scope, name.as_deref()))
}

#[tauri::command]
pub fn limits_set(
    scope: String,
    name: Option<String>,
    patch: LimitsPatchArg,
) -> Result<LimitsView, String> {
    set_in(&crate::mur_home_path(), &scope, name.as_deref(), patch)
}

#[tauri::command]
pub fn limits_remove_stale(
    scope: String,
    name: Option<String>,
    key: String,
) -> Result<LimitsView, String> {
    let home = crate::mur_home_path();
    remove_stale_key(&home, &target_of(&scope, name.as_deref())?, &key)
        .map_err(|e| format!("{e:#}"))?;
    Ok(resolve_in(&home, &scope, name.as_deref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full, loadable profile — the shipped agent-creation template with
    /// the name and model_ref swapped in. `AgentProfile` has many required
    /// (non-`#[serde(default)]`) fields; a hand-trimmed YAML silently fails
    /// to parse and `fleet_billing`'s `.ok()` turns that into "unknown",
    /// which would make every billing assertion below pass for the wrong
    /// reason.
    fn agent_profile_yaml(name: &str, model_ref: &str) -> String {
        let tmpl = std::fs::read_to_string(
            env!("CARGO_MANIFEST_DIR").to_string() + "/resources/mur-agent-template/profile.yaml",
        )
        .expect("shipped agent template");
        tmpl.replacen(
            "name: mur\n",
            &format!("name: {name}\nmodel_ref: {model_ref}\n"),
            1,
        )
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

    /// `ModelRegistry.schema_version` has no `#[serde(default)]`; a
    /// models.yaml missing it fails to parse and `fleet_billing`'s
    /// `ModelRegistry::load_from(..).ok()` silently turns that into "no
    /// registry" — every model then reads as unknown-billing (metered),
    /// which would make a "local model" test pass whether or not the
    /// billing plumbing actually worked.
    fn write_model_registry(home: &std::path::Path, model_ref: &str, billing: &str) {
        std::fs::write(
            home.join("models.yaml"),
            format!(
                "schema_version: 1\nmodels:\n  {model_ref}:\n    provider: openai\n    model: q\n    billing: {billing}\n"
            ),
        )
        .unwrap();
    }

    /// `fleet_billing` checks `router_or_concierge()` (defaults to "mur")
    /// AND every member — both must resolve, or the fleet reads as unknown
    /// (=> metered) regardless of what the members alone say.
    fn fleet_home(deadline: Option<&str>, billing: &str) -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        let h = t.path();
        std::fs::create_dir_all(h.join("fleets/dev")).unwrap();
        let limits = deadline
            .map(|d| format!("limits:\n  deadline: {d}\n"))
            .unwrap_or_default();
        std::fs::write(
            h.join("fleets/dev/fleet.yaml"),
            format!("name: dev\ngoal: g\nchannel_id: fleet-dev\nmembers: [pm]\n{limits}"),
        )
        .unwrap();
        write_model_registry(h, "m", billing);
        write_agent(h, "pm", "m");
        write_agent(h, "mur", "m"); // the fleet's router_or_concierge()
        t
    }

    /// §5 made visible: a local fleet is bounded by its (built-in) deadline;
    /// a billable fleet with no cap is bounded by deadline only — amber; a
    /// block that does not parse is unbounded and the view says why.
    #[test]
    fn bounded_badge_follows_the_resolver() {
        let t = fleet_home(None, "local");
        let v = resolve_in(t.path(), "fleet", Some("dev"));
        assert_eq!(v.bounded, "bounded");
        assert!(!v.billable);

        let t = fleet_home(Some("2h"), "usage_billed");
        let v = resolve_in(t.path(), "fleet", Some("dev"));
        assert_eq!(v.bounded, "deadline_only");
        assert!(
            v.rows
                .iter()
                .any(|r| r.knob == "cost_usd" && r.applies && r.value == "—")
        );

        let t = fleet_home(Some("soon"), "local");
        let v = resolve_in(t.path(), "fleet", Some("dev"));
        assert_eq!(v.bounded, "unbounded");
        assert!(v.error.as_deref().unwrap_or("").contains("limits.deadline"));
        assert!(v.rows.is_empty());
    }

    /// set writes ONE scope and returns the fresh view; unset deletes the key
    /// so the row goes back to inherited (local=false).
    #[test]
    fn set_and_unset_round_trip_through_the_view() {
        let t = fleet_home(None, "local");
        let v = set_in(
            t.path(),
            "fleet",
            Some("dev"),
            LimitsPatchArg {
                deadline: Some("3h".into()),
                stuck: None,
                cost_usd: None,
                unset: vec![],
            },
        )
        .unwrap();
        let d = v.rows.iter().find(|r| r.knob == "deadline").unwrap();
        assert!(
            d.local && d.raw.as_deref() == Some("3h") && d.value == "3h",
            "{d:?}",
        );

        let v = set_in(
            t.path(),
            "fleet",
            Some("dev"),
            LimitsPatchArg {
                deadline: None,
                stuck: None,
                cost_usd: None,
                unset: vec!["deadline".into()],
            },
        )
        .unwrap();
        let d = v.rows.iter().find(|r| r.knob == "deadline").unwrap();
        assert!(!d.local && d.source == "built-in default", "{d:?}");

        let e = set_in(
            t.path(),
            "fleet",
            Some("dev"),
            LimitsPatchArg {
                deadline: Some("soon".into()),
                stuck: None,
                cost_usd: None,
                unset: vec![],
            },
        )
        .unwrap_err();
        assert!(e.contains("limits.deadline"), "the CLI parser's words: {e}");
    }

    #[test]
    fn global_scope_edits_config_yaml_as_text() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(
            t.path().join("config.yaml"),
            "research_gateway:\n  key: k\n",
        )
        .unwrap();
        let v = set_in(
            t.path(),
            "global",
            None,
            LimitsPatchArg {
                deadline: None,
                stuck: Some("15m".into()),
                cost_usd: None,
                unset: vec![],
            },
        )
        .unwrap();
        assert!(
            v.rows
                .iter()
                .any(|r| r.knob == "stuck" && r.local && r.value == "15m")
        );
        let text = std::fs::read_to_string(t.path().join("config.yaml")).unwrap();
        assert!(
            text.contains("research_gateway:\n  key: k\n")
                && text.contains("limits:\n  stuck: 15m\n"),
            "{text}"
        );
    }

    /// `remove_stale` deletes the legacy key and returns the fresh view.
    #[test]
    fn remove_stale_returns_the_fresh_view() {
        let t = tempfile::tempdir().unwrap();
        let h = t.path();
        std::fs::create_dir_all(h.join("fleets/dev")).unwrap();
        std::fs::write(
            h.join("fleets/dev/fleet.yaml"),
            "name: dev\ngoal: g\nchannel_id: fleet-dev\nmembers: [pm]\nloop:\n  trigger: manual\n  max_iterations: 8\n  budget_usd: 0.0\n  deadline: ''\n  done_when: ''\n",
        )
        .unwrap();
        let v = resolve_in(h, "fleet", Some("dev"));
        assert_eq!(v.stale[0].key, "loop.max_iterations");
        let home = h.to_path_buf();
        unsafe { std::env::set_var("MUR_HOME", &home) };
        let v = limits_remove_stale(
            "fleet".into(),
            Some("dev".into()),
            "loop.max_iterations".into(),
        )
        .unwrap();
        unsafe { std::env::remove_var("MUR_HOME") };
        assert!(v.stale.is_empty(), "{:?}", v.stale);
    }
}
