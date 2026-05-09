//! SlackSocketConn — fetches a Socket Mode WSS URL and manages reconnect backoff.

use std::time::Duration;

use crate::bridge::slack::SlackError;

const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
const DEFAULT_BASE_URL: &str = "https://slack.com";

pub struct SlackSocketConn {
    pub app_token: String,
    pub backoff: Duration,
    base_url: String,
}

impl SlackSocketConn {
    pub fn new(app_token: String) -> Self {
        Self {
            app_token,
            backoff: MIN_BACKOFF,
            base_url: DEFAULT_BASE_URL.into(),
        }
    }

    pub fn new_with_base_url(app_token: String, base_url: String) -> Self {
        Self {
            app_token,
            backoff: MIN_BACKOFF,
            base_url,
        }
    }

    pub async fn open_wss_url(&self, client: &reqwest::Client) -> Result<String, SlackError> {
        let url = format!("{}/api/apps.connections.open", self.base_url);
        let resp = client
            .post(&url)
            .bearer_auth(&self.app_token)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send()
            .await
            .map_err(|e| SlackError::Network(e.to_string()))?;

        let status = resp.status().as_u16();
        if status == 401 {
            return Err(SlackError::Auth(401));
        }
        if !resp.status().is_success() {
            return Err(SlackError::Network(format!(
                "apps.connections.open HTTP {status}"
            )));
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| SlackError::Parse(e.to_string()))?;

        if !body["ok"].as_bool().unwrap_or(false) {
            let code = body["error"].as_str().unwrap_or("unknown");
            return Err(SlackError::Network(format!(
                "apps.connections.open returned ok=false: {code}"
            )));
        }

        body["url"].as_str().map(|s| s.to_string()).ok_or_else(|| {
            SlackError::Parse("missing `url` in apps.connections.open response".into())
        })
    }

    pub fn advance_backoff(&mut self) {
        self.backoff = (self.backoff * 2).min(MAX_BACKOFF);
    }

    pub fn reset_backoff(&mut self) {
        self.backoff = MIN_BACKOFF;
    }
}
