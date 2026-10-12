//! Which `profile.yaml` edits a running agent may pick up without a restart.
//!
//! One classifier for both sides: the runtime decides what to apply, the CLI
//! decides whether to say "next turn" or "restart". Two hand-kept lists would
//! drift, and the side that drifted would lie to the user.
//!
//! The comparison walks the serialized profile instead of naming fields, so a
//! field added to [`AgentProfile`] later lands in `cold` by default. That is
//! the safe direction: an unclassified field can only ever cost a restart, it
//! can never be hot-applied by accident — which matters because the locked set
//! includes entitlements and the sandbox.

use super::AgentProfile;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Dotted paths that take effect on the next turn.
///
/// `addons` is here for the skill gate; its MCP half is not hot (MCP servers
/// are spawned at boot), so a changed `addons` is reported in both lists.
pub const HOT_PATHS: &[&str] = &[
    "limits",
    "disabled_skills",
    "addons",
    "hitl.autonomy",
    "lifecycle.schedule",
    "lifecycle.idle_triggers",
];

/// Top-level keys whose hot paths are nested, so the walk descends into them.
const NESTED_PARENTS: &[&str] = &["hitl", "lifecycle"];

/// Bookkeeping stamps that change on every CLI write and mean nothing at runtime.
const IGNORED_PATHS: &[&str] = &["updated_at"];

/// The result of comparing two profiles.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReloadDiff {
    /// Changed paths a running agent applies on its next turn.
    pub hot: Vec<String>,
    /// Changed paths that need `mur agent restart`.
    pub cold: Vec<String>,
}

impl ReloadDiff {
    pub fn is_empty(&self) -> bool {
        self.hot.is_empty() && self.cold.is_empty()
    }

    /// True when every change is hot — nothing waits for a restart.
    pub fn hot_only(&self) -> bool {
        self.cold.is_empty()
    }
}

/// File in `agent_home` where a running agent records its reload state.
/// Written by the runtime (agent_home stays writable), read by `doctor`.
pub const RELOAD_STATE_FILE: &str = "reload_state.json";

/// What a running agent last did with its `profile.yaml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReloadState {
    /// Successful reloads since boot.
    pub generation: u64,
    /// Why the newest `profile.yaml` on disk is not the one being served.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reload_error: Option<String>,
    /// Same, for the global `limits:` in `config.yaml`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_limits_error: Option<String>,
    /// Changed paths that only take effect after `mur agent restart`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_cold_restart: Vec<String>,
}

impl ReloadState {
    /// Atomic write: temp file + rename.
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)
    }

    /// `None` when absent or unreadable — an agent that never reloaded.
    pub fn load(path: &std::path::Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }
}

impl AgentProfile {
    /// Classify what changed between `old` and `new`.
    pub fn reload_diff(old: &AgentProfile, new: &AgentProfile) -> ReloadDiff {
        let (Ok(a), Ok(b)) = (serde_json::to_value(old), serde_json::to_value(new)) else {
            // Unserializable is not a state the type can reach; if it ever
            // does, refusing to hot-apply anything is the safe answer.
            return ReloadDiff {
                hot: vec![],
                cold: vec!["<profile>".into()],
            };
        };
        let mut diff = ReloadDiff::default();
        walk(&a, &b, "", &mut diff);
        diff.hot.sort();
        diff.cold.sort();
        diff
    }
}

impl AgentProfile {
    /// `self` (the profile the process booted with) with only the [`HOT_PATHS`]
    /// fields taken from `newer`.
    ///
    /// How the runtime applies a reload: it never swaps in `newer` wholesale,
    /// so a changed locked field cannot reach a reader by any route — the
    /// running process keeps the boot value until a restart.
    pub fn with_hot_fields_from(&self, newer: &AgentProfile) -> AgentProfile {
        let mut out = self.clone();
        out.limits = newer.limits.clone();
        out.disabled_skills = newer.disabled_skills.clone();
        out.addons = newer.addons.clone();
        out.hitl.autonomy = newer.hitl.autonomy;
        out.lifecycle.schedule = newer.lifecycle.schedule.clone();
        out.lifecycle.idle_triggers = newer.lifecycle.idle_triggers.clone();
        out
    }
}

fn walk(a: &Value, b: &Value, prefix: &str, diff: &mut ReloadDiff) {
    let null = Value::Null;
    let (Some(ma), Some(mb)) = (a.as_object(), b.as_object()) else {
        if a != b {
            diff.cold.push(prefix.to_string());
        }
        return;
    };
    let mut keys: Vec<&String> = ma.keys().chain(mb.keys()).collect();
    keys.sort();
    keys.dedup();
    for k in keys {
        let path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        let (va, vb) = (ma.get(k).unwrap_or(&null), mb.get(k).unwrap_or(&null));
        if va == vb || IGNORED_PATHS.contains(&path.as_str()) {
            continue;
        }
        if HOT_PATHS.contains(&path.as_str()) {
            diff.hot.push(path.clone());
            if path == "addons" {
                diff.cold.push("addons (mcp servers)".into());
            }
        } else if prefix.is_empty() && NESTED_PARENTS.contains(&k.as_str()) {
            walk(va, vb, &path, diff);
        } else {
            diff.cold.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ScheduleEntry;

    fn base() -> AgentProfile {
        AgentProfile::default_for_tests()
    }

    #[test]
    fn identical_profiles_have_no_diff() {
        assert!(AgentProfile::reload_diff(&base(), &base()).is_empty());
    }

    #[test]
    fn limits_and_autonomy_are_hot() {
        let mut new = base();
        new.limits = Some(crate::limits::Limits {
            deadline: Some("5m".into()),
            ..Default::default()
        });
        new.hitl.autonomy = Some(crate::hitl::Autonomy::Continue);
        let d = AgentProfile::reload_diff(&base(), &new);
        assert_eq!(d.hot, vec!["hitl.autonomy", "limits"]);
        assert!(d.hot_only(), "{d:?}");
    }

    #[test]
    fn schedule_is_hot_but_restart_policy_is_cold() {
        let mut new = base();
        new.lifecycle.schedule.push(ScheduleEntry {
            cron: "0 9 * * *".into(),
            message: "hi".into(),
            sends_to: None,
            not_after: None,
        });
        new.lifecycle.max_restarts += 1;
        let d = AgentProfile::reload_diff(&base(), &new);
        assert_eq!(d.hot, vec!["lifecycle.schedule"]);
        assert_eq!(d.cold, vec!["lifecycle.max_restarts"]);
    }

    #[test]
    fn locked_fields_are_cold() {
        let mut new = base();
        new.name = "other".into();
        new.entitlements
            .network
            .outbound
            .allow_hosts
            .push("example.com".into());
        new.disabled_mcp.push("x".into());
        let d = AgentProfile::reload_diff(&base(), &new);
        assert!(d.hot.is_empty(), "{d:?}");
        assert!(d.cold.iter().any(|p| p == "name"), "{d:?}");
        assert!(
            d.cold.iter().any(|p| p.starts_with("entitlements")),
            "{d:?}"
        );
        assert!(d.cold.iter().any(|p| p == "disabled_mcp"), "{d:?}");
    }

    #[test]
    fn updated_at_alone_is_not_a_change() {
        let mut new = base();
        new.updated_at = "2099-01-01T00:00:00Z".into();
        assert!(AgentProfile::reload_diff(&base(), &new).is_empty());
    }

    #[test]
    fn overlay_applies_every_hot_path_and_nothing_else() {
        let mut new = base();
        new.limits = Some(crate::limits::Limits {
            stuck: Some("off".into()),
            ..Default::default()
        });
        new.disabled_skills.push("foo".into());
        new.hitl.autonomy = Some(crate::hitl::Autonomy::Continue);
        new.lifecycle.idle_triggers.push(crate::agent::IdleTrigger {
            after_secs: 60,
            message: "m".into(),
            sends_to: None,
            cooldown_secs: 600,
            respect_quiet_hours: false,
        });
        new.name = "renamed".into();
        let applied = base().with_hot_fields_from(&new);
        let rest = AgentProfile::reload_diff(&applied, &new);
        // Everything hot made it across; the locked rename did not.
        assert!(rest.hot.is_empty(), "{rest:?}");
        assert_eq!(rest.cold, vec!["name"]);
    }

    #[test]
    fn disabled_skills_is_hot() {
        let mut new = base();
        new.disabled_skills.push("foo".into());
        let d = AgentProfile::reload_diff(&base(), &new);
        assert_eq!(d.hot, vec!["disabled_skills"]);
        assert!(d.cold.is_empty());
    }
}
