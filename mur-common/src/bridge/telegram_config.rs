//! Track C2 — Telegram bridge configuration schema.
//!
//! `TelegramConfig` lives alongside an agent's `routes.yaml` (typically as
//! `telegram.yaml`) and captures everything needed for the runtime to long-poll
//! Telegram and route messages to the C1 bridge plumbing. Bot tokens are
//! **never** stored in this struct — only the macOS Keychain account name that
//! references them (`bot_token_keychain_account`).
//!
//! `PrivacyMode` controls whether the bridge accepts updates from groups in
//! addition to direct messages. The default is `DmOnly` — the safest setting
//! for a personal companion. `AllowGroups` requires `allow_groups[]` to be
//! populated explicitly.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Whether the bridge accepts updates from groups in addition to direct
/// messages. Default = `DmOnly`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyMode {
    /// Only direct messages from `chat_id` are accepted; group updates are
    /// dropped at the bridge edge.
    #[default]
    DmOnly,
    /// Group updates are accepted, but only from `chat_id` values listed in
    /// `allow_groups[]` on the parent `TelegramConfig`.
    AllowGroups,
}

/// Telegram-specific bridge configuration.
///
/// The bot token is **not** stored here; only the keychain account name. At
/// runtime the bridge resolves `bot_token_keychain_account` against the macOS
/// Keychain (or the Linux/Windows native equivalents) and keeps the secret in
/// memory only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelegramConfig {
    /// Public bot username from BotFather (e.g. `MyAgentBot`).
    pub bot_username: String,
    /// Keychain account name where the bot token is stored. The service is
    /// `mur-agent`; the account format is conventionally
    /// `{bridge_id}/telegram_bot_token`.
    pub bot_token_keychain_account: String,
    /// Telegram chat ID for the primary (DM) chat the bridge serves.
    pub chat_id: i64,
    /// Privacy gate for inbound updates (default `DmOnly`).
    #[serde(default)]
    pub privacy_mode: PrivacyMode,
    /// Group chat IDs allowed when `privacy_mode = AllowGroups`. Ignored when
    /// `privacy_mode = DmOnly`.
    #[serde(default)]
    pub allow_groups: Vec<i64>,
    /// Timestamp at which the operator acknowledged the Telegram E2E
    /// disclosure (Telegram DMs are not E2E encrypted by default). `None`
    /// means the disclosure has not been acknowledged yet — the runtime can
    /// use this to gate first-run flows.
    #[serde(default)]
    pub e2e_disclosure_acked_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn round_trips_yaml() {
        let cfg = TelegramConfig {
            bot_username: "MyAgentBot".into(),
            bot_token_keychain_account: "tg-bridge-1/telegram_bot_token".into(),
            chat_id: 123456789,
            privacy_mode: PrivacyMode::DmOnly,
            allow_groups: vec![],
            e2e_disclosure_acked_at: Some(Utc.with_ymd_and_hms(2026, 5, 4, 0, 0, 0).unwrap()),
        };
        let s = serde_yaml_ng::to_string(&cfg).unwrap();
        let back: TelegramConfig = serde_yaml_ng::from_str(&s).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn default_privacy_is_dm_only() {
        let s = "bot_username: bot\nbot_token_keychain_account: a\nchat_id: 1\n";
        let cfg: TelegramConfig = serde_yaml_ng::from_str(s).unwrap();
        assert_eq!(cfg.privacy_mode, PrivacyMode::DmOnly);
    }

    #[test]
    fn allow_groups_deserialize() {
        let s = "bot_username: b\nbot_token_keychain_account: a\nchat_id: 1\nprivacy_mode: allow_groups\nallow_groups: [-1001, -1002]\n";
        let cfg: TelegramConfig = serde_yaml_ng::from_str(s).unwrap();
        assert_eq!(cfg.privacy_mode, PrivacyMode::AllowGroups);
        assert_eq!(cfg.allow_groups, vec![-1001, -1002]);
    }

    #[test]
    fn ack_chrono_parse() {
        let s = "bot_username: b\nbot_token_keychain_account: a\nchat_id: 1\ne2e_disclosure_acked_at: 2026-05-04T00:00:00Z\n";
        let cfg: TelegramConfig = serde_yaml_ng::from_str(s).unwrap();
        assert!(cfg.e2e_disclosure_acked_at.is_some());
    }

    #[test]
    fn missing_token_account_errors() {
        let s = "bot_username: b\nchat_id: 1\n";
        let r: Result<TelegramConfig, _> = serde_yaml_ng::from_str(s);
        assert!(r.is_err());
    }
}
