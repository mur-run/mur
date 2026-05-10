use reqwest::Client;

use crate::bridge::slack::SlackError;

/// Send a message to a Slack channel via `chat.postMessage`.
pub async fn post_message(
    _client: &Client,
    _bot_token: &str,
    _channel: &str,
    _text: &str,
    _thread_ts: Option<&str>,
) -> Result<(), SlackError> {
    unimplemented!("post_message implemented in M-c7.5")
}
