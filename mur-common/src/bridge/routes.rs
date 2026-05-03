//! `routes.yaml` schema for the A2A bridge.
//!
//! A bridge is an LLM-less mur agent that ferries chat-platform messages to
//! the right *user* agent on the local A2A bus. `BridgeRouteConfig` describes
//! a deterministic mapping from inbound message → recipient agent(s) with the
//! precedence: explicit mention > platform-specific match (chat_id) >
//! default_route. There is **no LLM triage** in routing — the resolver is a
//! pure function over the inbound envelope and the static config.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeRouteConfig {
    pub default_route: String,
    #[serde(default)]
    pub routes: Vec<RouteEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteEntry {
    #[serde(rename = "match")]
    pub match_: RouteMatch,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fanout: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteMatch {
    pub platform: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mention: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    const SAMPLE: &str = r#"
default_route: coach
routes:
  - match: { platform: telegram, mention: "@coach" }
    agent: coach
  - match: { platform: telegram, chat_id: "12345" }
    agent: therapist
  - match: { platform: telegram, chat_id: "67890" }
    agent: coach
    fanout: [coach, journal_agent]
"#;
    #[test]
    fn parses_full_example() {
        let cfg: BridgeRouteConfig = serde_yaml_ng::from_str(SAMPLE).unwrap();
        assert_eq!(cfg.default_route, "coach");
        assert_eq!(cfg.routes.len(), 3);
    }
    #[test]
    fn round_trip_preserves_fields() {
        let cfg: BridgeRouteConfig = serde_yaml_ng::from_str(SAMPLE).unwrap();
        let s = serde_yaml_ng::to_string(&cfg).unwrap();
        assert_eq!(serde_yaml_ng::from_str::<BridgeRouteConfig>(&s).unwrap(), cfg);
    }
}
