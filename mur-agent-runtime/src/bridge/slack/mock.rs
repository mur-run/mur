//! Test doubles for the C7 Slack bridge.

use std::sync::Mutex;

use crate::bridge::slack::SlackError;
use crate::bridge::slack::inbound::SlackBotLike;

#[derive(Debug, Clone)]
pub struct MockSlackMessage {
    pub channel: String,
    pub text: String,
    pub thread_ts: Option<String>,
}

pub struct MockSlackBot {
    pub sent: Mutex<Vec<MockSlackMessage>>,
    pub post_message_err: Option<SlackError>,
    pub bot_user_id: String,
}

impl MockSlackBot {
    pub fn new() -> Self {
        Self {
            sent: Mutex::new(Vec::new()),
            post_message_err: None,
            bot_user_id: "U_BOT_TEST".into(),
        }
    }

    pub fn sent_messages(&self) -> Vec<MockSlackMessage> {
        self.sent.lock().unwrap().clone()
    }
}

impl Default for MockSlackBot {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl SlackBotLike for MockSlackBot {
    async fn post_message(
        &self,
        channel: &str,
        text: &str,
        thread_ts: Option<&str>,
    ) -> Result<(), SlackError> {
        if let Some(ref err) = self.post_message_err {
            return Err(match err {
                SlackError::Auth(c) => SlackError::Auth(*c),
                SlackError::RateLimit(d) => SlackError::RateLimit(*d),
                SlackError::Network(s) => SlackError::Network(s.clone()),
                SlackError::Parse(s) => SlackError::Parse(s.clone()),
                SlackError::WebSocket(s) => SlackError::WebSocket(s.clone()),
            });
        }
        self.sent.lock().unwrap().push(MockSlackMessage {
            channel: channel.to_string(),
            text: text.to_string(),
            thread_ts: thread_ts.map(|s| s.to_string()),
        });
        Ok(())
    }

    async fn auth_test(&self) -> Result<String, SlackError> {
        Ok(self.bot_user_id.clone())
    }
}

pub struct MockUserAgentHandle {
    pub received: Mutex<Vec<serde_json::Value>>,
    pub status: u16,
    pub reply_text: String,
}

impl MockUserAgentHandle {
    pub fn new(status: u16, reply_text: impl Into<String>) -> Self {
        Self {
            received: Mutex::new(Vec::new()),
            status,
            reply_text: reply_text.into(),
        }
    }

    pub fn ok(reply_text: impl Into<String>) -> Self {
        Self::new(200, reply_text)
    }

    pub fn server_error() -> Self {
        Self::new(500, "")
    }

    pub fn forward(&self, payload: serde_json::Value) -> (u16, String) {
        self.received.lock().unwrap().push(payload);
        (self.status, self.reply_text.clone())
    }
}
