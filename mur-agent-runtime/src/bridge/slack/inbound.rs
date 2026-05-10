//! Slack inbound loop — processes Socket Mode `events_api` envelopes.

use std::path::PathBuf;

use mur_common::bridge::SlackConfig;
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
    pub(crate) deps: Option<InboundDeps>,
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
