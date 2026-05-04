//! `LlmEntitlement` — controls whether an agent's supervisor is permitted to
//! construct an LLM client.
//!
//! Bridges (chat-platform connectors built on the agent runtime) are
//! intentionally LLM-less: their job is to relay messages between an external
//! transport (Slack, Discord, Telegram, etc.) and the A2A bus. They set
//! `entitlements.llm.mode = off` so the supervisor refuses to construct an
//! LLM client at startup. Default = `Allowed` for backward-compat with
//! existing agent profiles that pre-date this schema.
//!
//! See `docs/superpowers/plans/2026-05-03-mur-agent-track-c1-a2a-bridge.md`
//! task M-c1.0 for context.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LlmMode {
    #[default]
    Allowed,
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LlmEntitlement {
    #[serde(default)]
    pub mode: LlmMode,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_mode_is_allowed() {
        let e: LlmEntitlement = serde_yaml_ng::from_str("{}").unwrap();
        assert_eq!(e.mode, LlmMode::Allowed);
    }
    #[test]
    fn mode_off_round_trips() {
        let e = LlmEntitlement { mode: LlmMode::Off };
        let s = serde_yaml_ng::to_string(&e).unwrap();
        assert!(s.contains("mode: off"));
        assert_eq!(serde_yaml_ng::from_str::<LlmEntitlement>(&s).unwrap(), e);
    }
}
