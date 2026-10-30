//! The `bindings:` section of `mur browser status` (#2199): which saved
//! profile each agent's live `browser` entry launches with, flagged when the
//! binding cannot work. Warnings come from [`plan_bind`], the same planner
//! the binder refuses with, plus the profile files themselves.
//!
//! Best-effort like the rest of `status`: an unreadable agent profile or
//! browser profile marks its own row and never fails the command.

use std::path::Path;

use mur_browser::paths;
use mur_common::AgentProfile;

use super::bind_plan::{plan_bind, profile_domains, warning};
use super::live_entry::{LIVE_ENTRY, attached_profile};

/// `bindings:` header plus one row per agent that has a live entry.
pub(super) fn section(home: &Path) -> Vec<String> {
    let rows = rows(home);
    if rows.is_empty() {
        return vec!["bindings: (none)".into()];
    }
    let agent_w = rows.iter().map(|r| r.agent.len()).max().unwrap_or(0);
    let mut out = vec!["bindings:".to_string()];
    out.extend(rows.iter().map(|r| {
        let warn = r
            .warning
            .as_deref()
            .map(|w| format!("  ⚠ {w}"))
            .unwrap_or_default();
        format!("  {:agent_w$}  → {}{warn}", r.agent, r.profile)
    }));
    out
}

#[derive(Debug, PartialEq, Eq)]
struct Row {
    agent: String,
    /// Attached profile, or `none`; `?` when the agent profile is unreadable.
    profile: String,
    warning: Option<String>,
}

fn rows(home: &Path) -> Vec<Row> {
    let Ok(dir) = std::fs::read_dir(home.join("agents")) else {
        return Vec::new();
    };
    let mut agents: Vec<String> = dir
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.starts_with('.'))
        .collect();
    agents.sort();
    agents
        .into_iter()
        .filter_map(|agent| row(home, agent))
        .collect()
}

fn row(home: &Path, agent: String) -> Option<Row> {
    let path = home.join("agents").join(&agent).join("profile.yaml");
    let profile: AgentProfile = match std::fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|y| serde_yaml_ng::from_str(&y).map_err(|e| e.to_string()))
    {
        Ok(p) => p,
        // No profile.yaml: not an agent directory, nothing to bind.
        Err(_) if !path.exists() => return None,
        Err(error) => {
            return Some(Row {
                agent,
                profile: "?".into(),
                warning: Some(format!("agent profile unreadable: {error}")),
            });
        }
    };
    let entry = profile.mcp_servers.iter().find(|e| e.name == LIVE_ENTRY)?;
    let Some(site) = attached_profile(entry) else {
        return Some(Row {
            agent,
            profile: "none".into(),
            warning: None,
        });
    };
    let warning = profile_warning(home, site).or_else(|| {
        let meta = super::read_profile_meta(&paths::profile_meta(home, site)).ok()?;
        let domains = profile_domains(&meta)?;
        warning(&plan_bind(Some(entry), site, &domains))
    });
    Some(Row {
        agent,
        profile: site.to_owned(),
        warning,
    })
}

/// Why the attached profile `site` cannot be used at all, before any host check.
fn profile_warning(home: &Path, site: &str) -> Option<String> {
    if paths::validate_name(site).is_err() {
        return Some("invalid profile name".into());
    }
    if !paths::profile_dir(home, site).is_dir() {
        return Some("profile missing".into());
    }
    if !paths::profile_state(home, site).is_file() {
        return Some("profile incomplete".into());
    }
    let meta_path = paths::profile_meta(home, site);
    if !meta_path.is_file() {
        return Some("profile metadata missing".into());
    }
    match super::read_profile_meta(&meta_path) {
        Err(error) => Some(format!("profile metadata unreadable: {error}")),
        Ok(meta) if profile_domains(&meta).is_none() => Some("profile names no host".into()),
        Ok(_) => None,
    }
}

#[cfg(test)]
#[path = "bindings_tests.rs"]
mod tests;
