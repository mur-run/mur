// Stub — fully implemented in M-c7.5

use reqwest::Client;

use crate::bridge::slack::SlackError;

/// Send a message to a Slack channel via `chat.postMessage`.
/// Stub: will be fully implemented in M-c7.5.
pub async fn post_message(
    _client: &Client,
    _bot_token: &str,
    _channel: &str,
    _text: &str,
    _thread_ts: Option<&str>,
) -> Result<(), SlackError> {
    unimplemented!("post_message implemented in M-c7.5")
}
