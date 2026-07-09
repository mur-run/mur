// mur-research-gateway/src/fetcher.rs
use crate::net_guard::{self, GuardReject};
use serde::Serialize;
use std::time::Duration;

const MAX_BODY_BYTES: usize = 5 * 1024 * 1024; // ponytail: 5MB cap; config if a real doc exceeds it

#[derive(Debug, Serialize)]
pub struct FetchResult {
    pub url: String,
    pub status: u16,
    pub title: Option<String>,
    pub text: String,
    pub tier: u8,
}

#[derive(Debug)]
pub enum FetchError {
    Guard(GuardReject),
    Http(String),
    TooLarge,
}

/// Screen `url` off the tokio worker thread. `screen_url` calls the blocking
/// `to_socket_addrs()` (synchronous DNS resolution); running it inline on an
/// async fn would stall the worker for the duration of the resolve. `deny` is
/// cloned into the blocking task since `spawn_blocking` requires `'static`.
async fn screen_url_blocking(url: &str, deny: &[String]) -> Result<url::Url, GuardReject> {
    let url = url.to_string();
    let deny = deny.to_vec();
    tokio::task::spawn_blocking(move || net_guard::screen_url(&url, &deny))
        .await
        .unwrap_or(Err(GuardReject::Unresolvable)) // join error (panic) — treat as unresolvable, fail closed
}

pub async fn fetch_tier1(
    url: &str,
    deny: &[String],
    timeout: Duration,
) -> Result<FetchResult, FetchError> {
    let screened = screen_url_blocking(url, deny)
        .await
        .map_err(FetchError::Guard)?;
    // ponytail: reqwest re-resolves internally; screen_url already rejected
    // private targets. A pinned-IP resolver closes the rebinding window fully —
    // upgrade to reqwest::dns::Resolve if the advisory window matters.
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none()) // no auto-redirect: each hop must be re-screened by the worker
        .build()
        .map_err(|e| FetchError::Http(e.to_string()))?;
    let resp = client
        .get(screened.clone())
        .send()
        .await
        .map_err(|e| FetchError::Http(e.to_string()))?;
    let status = resp.status().as_u16();
    let body = resp
        .text()
        .await
        .map_err(|e| FetchError::Http(e.to_string()))?;
    if body.len() > MAX_BODY_BYTES {
        return Err(FetchError::TooLarge);
    }
    let title = extract_title(&body);
    Ok(FetchResult {
        url: screened.to_string(),
        status,
        title,
        text: html_to_text(&body),
        tier: 1,
    })
}

fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title>")? + 7;
    let end = lower[start..].find("</title>")? + start;
    Some(html[start..end].trim().to_string())
}

// ponytail: naive tag-strip. Good enough for claim extraction; swap for a real
// readability crate only if extraction quality measurably suffers.
fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn refuses_private_target() {
        let e = fetch_tier1("http://127.0.0.1:1/", &[], Duration::from_secs(2))
            .await
            .unwrap_err();
        assert!(matches!(e, FetchError::Guard(_)));
    }
    #[tokio::test]
    async fn refuses_denied_host() {
        let e = fetch_tier1(
            "http://blocked.example/",
            &["blocked.example".into()],
            Duration::from_secs(2),
        )
        .await
        .unwrap_err();
        assert!(matches!(e, FetchError::Guard(GuardReject::DeniedHost)));
    }
}
