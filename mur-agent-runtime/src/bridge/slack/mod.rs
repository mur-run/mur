//! Track C7 — Slack bridge (Socket Mode).
//!
//! A zero-LLM bridge agent that connects to Slack via Socket Mode
//! WebSocket, dedupes events, signs envelopes, forwards to the user
//! agent via A2A, and replies via chat.postMessage.

pub mod inbound;
pub mod mock;
pub mod reply;
pub mod socket;

pub use inbound::{InboundDeps, SlackBotLike, SlackInboundLoop};
pub use mock::{MockSlackBot, MockSlackMessage, MockUserAgentHandle};
pub use socket::SlackSocketConn;

/// Local error type for all Slack bridge operations.
#[derive(Debug, Clone, thiserror::Error)]
pub enum SlackError {
    #[error("Slack auth error (HTTP {0}): check your tokens")]
    Auth(u16),
    #[error("Slack rate limit — retry after {0:?}")]
    RateLimit(std::time::Duration),
    #[error("Slack network error: {0}")]
    Network(String),
    #[error("Slack parse error: {0}")]
    Parse(String),
    #[error("WebSocket error: {0}")]
    WebSocket(String),
}
