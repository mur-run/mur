//! Slack bridge configuration (stored in agent profile.yaml `bridge:` block).

use serde::{Deserialize, Serialize};

/// Config block for a Slack bridge agent.
/// Tokens are stored in the system keychain; the account names below
/// are pointers, not the secrets themselves.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlackConfig {
    /// Human-readable workspace URL shown in error messages and logs.
    pub workspace_url: String,
    /// Keychain account name for the xoxb-… Bot Token.
    pub bot_token_keychain_account: String,
    /// Keychain account name for the xapp-… App Token (Socket Mode).
    pub app_token_keychain_account: String,
    /// Privacy gate: which Slack event types reach the user agent.
    #[serde(default)]
    pub privacy_mode: SlackPrivacyMode,
    /// Allowed Slack channel IDs (C…). Empty = all channels allowed.
    #[serde(default)]
    pub allowed_channels: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SlackPrivacyMode {
    /// Only DMs to the bot reach the agent. Channel mentions are dropped.
    DmOnly,
    /// DMs and @mentions in channels both reach the agent (default).
    #[default]
    DmAndMentions,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_yaml() {
        let yaml = r#"
workspace_url: "https://myteam.slack.com"
bot_token_keychain_account: "mur_slack_bot_myagent"
app_token_keychain_account: "mur_slack_app_myagent"
"#;
        let cfg: SlackConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.privacy_mode, SlackPrivacyMode::DmAndMentions);
        assert!(cfg.allowed_channels.is_empty());
    }

    #[test]
    fn dm_only_mode_round_trips() {
        let yaml = r#"
workspace_url: "https://myteam.slack.com"
bot_token_keychain_account: "mur_slack_bot_x"
app_token_keychain_account: "mur_slack_app_x"
privacy_mode: dm_only
"#;
        let cfg: SlackConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.privacy_mode, SlackPrivacyMode::DmOnly);
    }

    #[test]
    fn allowed_channels_round_trips() {
        let yaml = r#"
workspace_url: "https://myteam.slack.com"
bot_token_keychain_account: "mur_slack_bot_x"
app_token_keychain_account: "mur_slack_app_x"
allowed_channels: ["C111", "C222"]
"#;
        let cfg: SlackConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.allowed_channels, vec!["C111", "C222"]);
    }
}
