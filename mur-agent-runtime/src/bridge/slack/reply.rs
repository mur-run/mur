//! `chat.postMessage` helper with Retry-After rate-limit handling.

use std::time::Duration;

use reqwest::Client;

use crate::bridge::slack::SlackError;

/// Max retries on HTTP 429 (rate limited) before giving up.
const MAX_RETRIES: u32 = 3;
/// Default retry wait when Slack omits the `Retry-After` header.
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(5);

/// POST `chat.postMessage` to Slack.
///
/// On HTTP 429 reads `Retry-After` header (seconds) and retries up to
/// `MAX_RETRIES` times. After exhausting retries returns
/// `SlackError::RateLimit`. On HTTP 401 returns `SlackError::Auth`.
///
/// `base_url` overrides `https://slack.com` — used in tests to redirect to a
/// local mock server.
pub async fn post_message(
    client: &Client,
    bot_token: &str,
    channel: &str,
    text: &str,
    thread_ts: Option<&str>,
    base_url: Option<&str>,
) -> Result<(), SlackError> {
    let url = format!(
        "{}/api/chat.postMessage",
        base_url.unwrap_or("https://slack.com")
    );

    let mut body = serde_json::json!({
        "channel": channel,
        "text":    text,
    });
    if let Some(ts) = thread_ts {
        body["thread_ts"] = serde_json::json!(ts);
    }

    let mut attempts = 0u32;
    loop {
        let resp = client
            .post(&url)
            .bearer_auth(bot_token)
            .json(&body)
            .send()
            .await
            .map_err(|e| SlackError::Network(e.to_string()))?;

        let status = resp.status().as_u16();

        if status == 401 {
            return Err(SlackError::Auth(401));
        }

        if status == 429 {
            attempts += 1;
            if attempts > MAX_RETRIES {
                return Err(SlackError::RateLimit(DEFAULT_RETRY_AFTER));
            }
            let wait = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(Duration::from_secs)
                .unwrap_or(DEFAULT_RETRY_AFTER);
            tokio::time::sleep(wait).await;
            continue;
        }

        if !resp.status().is_success() {
            return Err(SlackError::Network(format!(
                "chat.postMessage HTTP {status}"
            )));
        }

        return Ok(());
    }
}
