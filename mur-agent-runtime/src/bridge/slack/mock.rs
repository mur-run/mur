// Stub — fully implemented in M-c7.2+
use crate::bridge::slack::inbound::SlackBotLike;

pub struct MockSlackBot;
impl SlackBotLike for MockSlackBot {}

#[derive(Debug, Clone)]
pub struct MockSlackMessage {
    pub channel: String,
    pub text: String,
    pub thread_ts: Option<String>,
}
