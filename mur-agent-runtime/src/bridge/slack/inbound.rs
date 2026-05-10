//! Slack inbound loop — processes Socket Mode `events_api` envelopes.

use std::path::PathBuf;

use mur_common::bridge::{SlackConfig, SlackPrivacyMode};
use mur_common::identity::AgentIdentity;

use crate::bridge::ack::AckTracker;
use crate::bridge::dedupe::DedupeStore;
use crate::bridge::slack::SlackError;
use crate::bridge::slack::mock::MockUserAgentHandle;

// ── Slack event wire types ────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SlackEnvelope {
    pub envelope_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub payload: Option<SlackEventPayload>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SlackEventPayload {
    pub event: SlackEvent,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SlackEvent {
    #[serde(rename = "type")]
    pub kind: String,
    pub user: Option<String>,
    pub text: Option<String>,
    pub ts: String,
    pub channel: String,
    pub channel_type: Option<String>,
    pub thread_ts: Option<String>,
}

// ── Bot trait + production type ───────────────────────────────────────────

#[async_trait::async_trait]
pub trait SlackBotLike: Send + Sync + 'static {
    async fn post_message(
        &self,
        channel: &str,
        text: &str,
        thread_ts: Option<&str>,
    ) -> Result<(), SlackError>;

    async fn auth_test(&self) -> Result<String, SlackError>;
}

pub struct RealSlackBot {
    pub(crate) client: reqwest::Client,
    pub(crate) bot_token: String,
}

#[async_trait::async_trait]
impl SlackBotLike for RealSlackBot {
    async fn post_message(
        &self,
        channel: &str,
        text: &str,
        thread_ts: Option<&str>,
    ) -> Result<(), SlackError> {
        crate::bridge::slack::reply::post_message(
            &self.client,
            &self.bot_token,
            channel,
            text,
            thread_ts,
            None,
        )
        .await
    }

    async fn auth_test(&self) -> Result<String, SlackError> {
        let resp = self
            .client
            .post("https://slack.com/api/auth.test")
            .bearer_auth(&self.bot_token)
            .send()
            .await
            .map_err(|e| SlackError::Network(e.to_string()))?;

        if resp.status().as_u16() == 401 {
            return Err(SlackError::Auth(401));
        }
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| SlackError::Parse(e.to_string()))?;
        body["user_id"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| SlackError::Parse("missing user_id in auth.test".into()))
    }
}

// ── InboundDeps ───────────────────────────────────────────────────────────

pub struct InboundDeps {
    pub config: SlackConfig,
    pub dedupe: DedupeStore,
    pub ack: AckTracker<String>,
    pub identity: AgentIdentity,
    pub key_version: u32,
    pub always_5xx: bool,
    pub user_agent: Option<MockUserAgentHandle>,
    pub agent_home: PathBuf,
}

// ── SlackInboundLoop ──────────────────────────────────────────────────────

pub struct SlackInboundLoop<B: SlackBotLike> {
    pub bot: B,
    pub deps: Option<InboundDeps>,
}

impl<B: SlackBotLike> SlackInboundLoop<B> {
    pub fn stub_new(bot: B) -> Self {
        Self { bot, deps: None }
    }

    pub fn new(bot: B, deps: InboundDeps) -> Self {
        Self {
            bot,
            deps: Some(deps),
        }
    }
}

/// Result of processing one Socket Mode envelope.
#[derive(Debug)]
pub struct TickResult {
    pub forwarded: bool,
}

impl<B: SlackBotLike> SlackInboundLoop<B> {
    /// Process one `events_api` envelope through phases 1-7 (classify, privacy,
    /// dedupe, strip mention, sign, forward, reply).
    pub async fn tick_once(&mut self, envelope: SlackEnvelope) -> Result<TickResult, SlackError> {
        let Some(payload) = envelope.payload else {
            return Ok(TickResult { forwarded: false });
        };
        let event = payload.event;

        let is_dm = event.channel_type.as_deref() == Some("im");
        let is_mention = event.kind == "app_mention";
        if !is_dm && !is_mention {
            return Ok(TickResult { forwarded: false });
        }

        let deps = self
            .deps
            .as_mut()
            .expect("tick_once called on stub_new loop");

        if is_mention && deps.config.privacy_mode == SlackPrivacyMode::DmOnly {
            return Ok(TickResult { forwarded: false });
        }
        if is_mention
            && !deps.config.allowed_channels.is_empty()
            && !deps.config.allowed_channels.contains(&event.channel)
        {
            return Ok(TickResult { forwarded: false });
        }

        let dedupe_key = format!("{}:{}", event.channel, event.ts);
        if deps.dedupe.is_seen(&dedupe_key).unwrap_or(false) {
            return Ok(TickResult { forwarded: false });
        }
        deps.dedupe
            .mark_seen(&dedupe_key)
            .map_err(|e| SlackError::Network(e.to_string()))?;

        // Phase 4: strip bot mention prefix "<@U…> " from text
        let raw_text = event.text.clone().unwrap_or_default();
        let text = if is_mention {
            if let Some(rest) = raw_text.split_once("> ") {
                rest.1.trim().to_string()
            } else {
                raw_text.trim().to_string()
            }
        } else {
            raw_text
        };

        // Phase 5: build + sign JSON payload using the bridge identity
        let payload_value = serde_json::json!({
            "text": text,
            "sender_slack_user_id": event.user.as_deref().unwrap_or(""),
            "channel": event.channel,
            "ts": event.ts,
            "thread_ts": event.thread_ts,
            "is_dm": is_dm,
        });
        let canonical =
            serde_json::to_vec(&payload_value).map_err(|e| SlackError::Parse(e.to_string()))?;
        let sig_bytes = deps.identity.sign_bytes(&canonical);
        let bridge_pubkey = deps.identity.public_key_multibase();

        let forwarded_payload = serde_json::json!({
            "payload": payload_value,
            "signature": hex::encode(sig_bytes),
            "bridge_pubkey_multibase": bridge_pubkey,
            "key_version": deps.key_version,
        });

        // Phase 6: forward to user agent + advance AckTracker
        let (status, reply_text) = if let Some(ref agent) = deps.user_agent {
            agent.forward(forwarded_payload)
        } else if deps.always_5xx {
            (500u16, String::new())
        } else {
            (200u16, String::new())
        };

        let did_forward = status / 100 == 2;
        if did_forward {
            deps.ack.start_pending(event.ts.clone());
            deps.ack.confirm();
        } else {
            tracing::warn!(
                channel = %event.channel,
                ts = %event.ts,
                status,
                "A2A forward failed — AckTracker not advanced"
            );
        }

        // Phase 7: post reply — in-thread for mentions, inline for DMs
        if did_forward && !reply_text.is_empty() {
            let thread_ts = if is_mention {
                Some(event.ts.as_str())
            } else {
                None
            };
            if let Err(e) = self
                .bot
                .post_message(&event.channel, &reply_text, thread_ts)
                .await
            {
                tracing::warn!("post_message failed: {e}");
            }
        }

        Ok(TickResult {
            forwarded: did_forward,
        })
    }
}
