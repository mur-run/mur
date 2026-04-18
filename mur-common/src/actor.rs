use serde::{Deserialize, Serialize};

/// The platform/tool that produced a signal or observation.
///
/// Kept intentionally narrow — new sources should be added here rather than
/// passed as freeform strings, so the set of valid origins is a compile-time concern.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorSource {
    ClaudeCode,
    Cursor,
    Aider,
    Slack,
    Telegram,
    Discord,
    CommanderDaemon,
    MurCli,
}

/// Provenance for a signal or pattern — records WHO produced it, without
/// trying to resolve identity to a canonical user.
///
/// - `source + native_id` is the authoritative tuple (e.g. `Slack + U123ABC`).
/// - `display_name` is a non-authoritative hint for humans reading YAML.
/// - `resolved_user_id` is filled by mur-server after lookup, stays `None` on the client.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Actor {
    pub source: ActorSource,
    pub native_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_user_id: Option<String>,
}

impl Actor {
    /// Stable dedupe key used as HashMap key in `Evidence.contributions`.
    /// Format: `"{Source:?}:{native_id}"` (e.g. `"Slack:U123ABC"`).
    pub fn key(&self) -> String {
        format!("{:?}:{}", self.source, self.native_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_format_slack() {
        let a = Actor {
            source: ActorSource::Slack,
            native_id: "U123ABC".into(),
            display_name: Some("alice".into()),
            resolved_user_id: None,
        };
        assert_eq!(a.key(), "Slack:U123ABC");
    }

    #[test]
    fn key_format_commander_daemon() {
        let a = Actor {
            source: ActorSource::CommanderDaemon,
            native_id: "svc-1".into(),
            display_name: None,
            resolved_user_id: None,
        };
        assert_eq!(a.key(), "CommanderDaemon:svc-1");
    }

    #[test]
    fn yaml_roundtrip_minimal() {
        let a = Actor {
            source: ActorSource::CommanderDaemon,
            native_id: "svc-1".into(),
            display_name: None,
            resolved_user_id: None,
        };
        let y = serde_yaml::to_string(&a).unwrap();
        let back: Actor = serde_yaml::from_str(&y).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn yaml_roundtrip_full() {
        let a = Actor {
            source: ActorSource::Slack,
            native_id: "U999".into(),
            display_name: Some("bob".into()),
            resolved_user_id: Some("user-uuid-123".into()),
        };
        let y = serde_yaml::to_string(&a).unwrap();
        let back: Actor = serde_yaml::from_str(&y).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn yaml_omits_none_fields() {
        let a = Actor {
            source: ActorSource::MurCli,
            native_id: "local".into(),
            display_name: None,
            resolved_user_id: None,
        };
        let y = serde_yaml::to_string(&a).unwrap();
        assert!(!y.contains("display_name"));
        assert!(!y.contains("resolved_user_id"));
    }

    #[test]
    fn actor_source_serializes_snake_case() {
        // ClaudeCode variant should serialize as "claude_code"
        let a = Actor {
            source: ActorSource::ClaudeCode,
            native_id: "x".into(),
            display_name: None,
            resolved_user_id: None,
        };
        let y = serde_yaml::to_string(&a).unwrap();
        assert!(y.contains("source: claude_code"));
    }
}
