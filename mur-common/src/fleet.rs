//! Fleet — a named squad of agents working a shared goal over one channel.

use serde::{Deserialize, Serialize};

pub const CONCIERGE_AGENT: &str = "mur";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fleet {
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub goal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router: Option<String>,
    #[serde(default)]
    pub members: Vec<String>,
    pub channel_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    #[serde(default, rename = "loop", skip_serializing_if = "Option::is_none")]
    pub loop_cfg: Option<FleetLoop>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetLoop {
    #[serde(default = "default_trigger")]
    pub trigger: String,
    // default 0 → resolvers fall back (cap → DEFAULT_MAX_ITERATIONS, budget → no cap),
    // so a minimal `loop:` block (e.g. just `trigger:`) deserializes.
    #[serde(default)]
    pub max_iterations: u32,
    #[serde(default)]
    pub budget_usd: f64,
    #[serde(default)]
    pub deadline: String,
    #[serde(default)]
    pub done_when: String,
}

fn default_trigger() -> String {
    "manual".to_string()
}

impl Fleet {
    pub fn router_or_concierge(&self) -> &str {
        self.router.as_deref().unwrap_or(CONCIERGE_AGENT)
    }
}

/// A fleet name must be a filesystem-safe lowercase slug (it becomes a directory
/// `~/.mur/fleets/<name>` and a channel id `fleet-<name>`).
pub fn valid_fleet_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_fleet_name_accepts_and_rejects() {
        // accepted
        assert!(valid_fleet_name("dev"));
        assert!(valid_fleet_name("dev-team"));
        assert!(valid_fleet_name("dev_1"));
        assert!(valid_fleet_name("ab12"));
        // rejected
        assert!(!valid_fleet_name("")); // empty
        assert!(!valid_fleet_name("../x")); // path traversal
        assert!(!valid_fleet_name("a/b")); // slash
        assert!(!valid_fleet_name("a\\b")); // backslash
        assert!(!valid_fleet_name("Dev")); // uppercase
        assert!(!valid_fleet_name("a b")); // space
        assert!(!valid_fleet_name(".hidden")); // dot
    }

    #[test]
    fn fleet_minimal_yaml_deserializes_with_defaults() {
        let f: Fleet = serde_yaml::from_str("name: dev\nchannel_id: fleet-dev\n").unwrap();
        assert_eq!(f.name, "dev");
        assert_eq!(f.channel_id, "fleet-dev");
        assert!(f.members.is_empty());
        assert_eq!(f.router_or_concierge(), CONCIERGE_AGENT);
        assert!(f.loop_cfg.is_none());
    }

    #[test]
    fn fleet_yaml_roundtrip_and_router_default() {
        let f = Fleet {
            name: "dev".into(),
            display_name: "Dev Team".into(),
            goal: "ship it".into(),
            router: None,
            members: vec!["pm".into(), "qa".into()],
            channel_id: "fleet-dev".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg: None,
        };
        assert_eq!(f.router_or_concierge(), CONCIERGE_AGENT);
        let yaml = serde_yaml::to_string(&f).unwrap();
        let back: Fleet = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back, f);
        // `loop:` key (not `loop_cfg`) when present
        let with_loop: Fleet = serde_yaml::from_str(
            "name: dev\ndisplay_name: Dev\ngoal: test\nchannel_id: fleet-dev\nrules: []\nskills: []\nmembers: []\nloop:\n  trigger: manual\n  max_iterations: 3\n  budget_usd: 1.0\n  deadline: '2026-12-31'\n  done_when: 'all_tasks_done'\n",
        ).unwrap();
        assert_eq!(with_loop.loop_cfg.unwrap().max_iterations, 3);
    }

    #[test]
    fn minimal_loop_block_deserializes_with_defaults() {
        // A `loop:` block with only a trigger must not fail (max_iterations /
        // budget_usd default to 0 → resolvers fall back).
        let f: Fleet = serde_yaml::from_str(
            "name: dev\nchannel_id: fleet-dev\nloop:\n  trigger: \"interval:1h\"\n",
        )
        .unwrap();
        let l = f.loop_cfg.unwrap();
        assert_eq!(l.trigger, "interval:1h");
        assert_eq!(l.max_iterations, 0);
        assert_eq!(l.budget_usd, 0.0);
    }
}
