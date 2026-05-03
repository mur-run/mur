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

#[derive(Debug, Clone)]
pub struct InboundMessage {
    pub platform: String,
    pub chat_id: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    primary: String,
    fanout: Vec<String>,
}

impl Resolution {
    pub fn recipients(&self) -> Vec<String> {
        if self.fanout.is_empty() {
            vec![self.primary.clone()]
        } else {
            self.fanout.clone()
        }
    }
}

impl BridgeRouteConfig {
    pub fn resolve(&self, inbound: &InboundMessage) -> Resolution {
        // Pass 1: mention (highest priority)
        for entry in &self.routes {
            if entry.match_.platform != inbound.platform {
                continue;
            }
            if let Some(m) = &entry.match_.mention
                && inbound.body.contains(m.as_str())
            {
                return Resolution {
                    primary: entry.agent.clone(),
                    fanout: entry.fanout.clone(),
                };
            }
        }
        // Pass 2: platform + chat_id (skip mention-routes; they only match in pass 1)
        for entry in &self.routes {
            if entry.match_.platform != inbound.platform {
                continue;
            }
            if entry.match_.mention.is_some() {
                continue;
            }
            if let Some(c) = &entry.match_.chat_id
                && c == &inbound.chat_id
            {
                return Resolution {
                    primary: entry.agent.clone(),
                    fanout: entry.fanout.clone(),
                };
            }
        }
        // Pass 3: default
        Resolution {
            primary: self.default_route.clone(),
            fanout: vec![],
        }
    }
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

    #[test]
    fn resolve_falls_back_to_default() {
        let cfg: BridgeRouteConfig = serde_yaml_ng::from_str(SAMPLE).unwrap();
        let r = cfg.resolve(&InboundMessage {
            platform: "telegram".into(),
            chat_id: "99999".into(),
            body: "hello".into(),
        });
        assert_eq!(r.recipients(), vec!["coach"]);
    }

    #[test]
    fn mention_wins_over_chat_id() {
        let cfg: BridgeRouteConfig = serde_yaml_ng::from_str(SAMPLE).unwrap();
        let r = cfg.resolve(&InboundMessage {
            platform: "telegram".into(),
            chat_id: "12345".into(), // would route to therapist
            body: "hey @coach help".into(), // mention wins
        });
        assert_eq!(r.recipients(), vec!["coach"]);
    }

    #[test]
    fn chat_id_when_no_mention() {
        let cfg: BridgeRouteConfig = serde_yaml_ng::from_str(SAMPLE).unwrap();
        let r = cfg.resolve(&InboundMessage {
            platform: "telegram".into(),
            chat_id: "12345".into(),
            body: "no mentions".into(),
        });
        assert_eq!(r.recipients(), vec!["therapist"]);
    }

    #[test]
    fn fanout_returns_full_list() {
        let cfg: BridgeRouteConfig = serde_yaml_ng::from_str(SAMPLE).unwrap();
        let r = cfg.resolve(&InboundMessage {
            platform: "telegram".into(),
            chat_id: "67890".into(),
            body: "ping".into(),
        });
        assert_eq!(r.recipients(), vec!["coach", "journal_agent"]);
    }

    #[test]
    fn platform_mismatch_falls_through() {
        let cfg: BridgeRouteConfig = serde_yaml_ng::from_str(SAMPLE).unwrap();
        let r = cfg.resolve(&InboundMessage {
            platform: "slack".into(),
            chat_id: "12345".into(),
            body: "ping".into(),
        });
        assert_eq!(r.recipients(), vec!["coach"]);
    }
}
