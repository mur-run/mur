// mur-research-gateway/src/fetcher.rs
use crate::net_guard::{self, GuardReject};
use serde::Serialize;
use std::time::Duration;

// pub(crate) so browser.rs (tiers 2/3) enforces the SAME body cap on
// agent-browser stdout that tier-1 enforces on the HTTP body.
pub(crate) const MAX_BODY_BYTES: usize = 5 * 1024 * 1024; // ponytail: 5MB cap; config if a real doc exceeds it

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
///
/// `pub(crate)` so `browser.rs` (tiers 2/3) can reuse the same off-thread
/// pre-spawn screen instead of duplicating the wrapper.
pub(crate) async fn screen_url_blocking(
    url: &str,
    deny: &[String],
) -> Result<url::Url, GuardReject> {
    let url = url.to_string();
    let deny = deny.to_vec();
    tokio::task::spawn_blocking(move || net_guard::screen_url(&url, &deny))
        .await
        .unwrap_or(Err(GuardReject::Unresolvable)) // join error (panic) — treat as unresolvable, fail closed
}

/// True if appending `chunk_len` bytes to a `current_len`-byte buffer would push
/// the running total past `max`. Extracted so the streaming cap decision is unit
/// testable without a live oversized HTTP body.
fn would_exceed(current_len: usize, chunk_len: usize, max: usize) -> bool {
    current_len.saturating_add(chunk_len) > max
}

pub async fn fetch_tier1(
    url: &str,
    deny: &[String],
    timeout: Duration,
) -> Result<FetchResult, FetchError> {
    let screened = screen_url_blocking(url, deny)
        .await
        .map_err(FetchError::Guard)?;
    // ADVISORY SSRF enforcement (egress-governance spec): screen_url screens the
    // IPs resolved AT SCREEN TIME, but reqwest re-resolves the host at connect
    // time — client.get(screened) does NOT pin to the screened IP — so a
    // DNS-rebinding window between screen and connect remains open. This is
    // acceptable per the spec's advisory tier; airtight pin-to-proxy is Phase 3.
    // TODO(Phase 3): pin the connection to the screened IP via a custom
    // reqwest::dns::Resolve to close the DNS-rebinding window.
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none()) // no auto-redirect: each hop must be re-screened by the worker
        .build()
        .map_err(|e| FetchError::Http(e.to_string()))?;
    let mut resp = client
        .get(screened.clone())
        .send()
        .await
        .map_err(|e| FetchError::Http(e.to_string()))?;
    let status = resp.status().as_u16();

    // Fast-reject on advertised size before reading a single body byte.
    if let Some(len) = resp.content_length()
        && len > MAX_BODY_BYTES as u64
    {
        return Err(FetchError::TooLarge);
    }

    // Stream and enforce the cap incrementally: a lying/absent Content-Length
    // must never let us buffer an unbounded body (resp.text() would OOM here).
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| FetchError::Http(e.to_string()))?
    {
        if would_exceed(buf.len(), chunk.len(), MAX_BODY_BYTES) {
            return Err(FetchError::TooLarge);
        }
        buf.extend_from_slice(&chunk);
    }

    let body = String::from_utf8_lossy(&buf);
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

    #[test]
    fn body_cap_would_exceed() {
        // Under the cap: appending stays within budget.
        assert!(!would_exceed(0, MAX_BODY_BYTES, MAX_BODY_BYTES));
        assert!(!would_exceed(MAX_BODY_BYTES - 10, 10, MAX_BODY_BYTES));
        // At/over the cap: one byte past the budget is rejected.
        assert!(would_exceed(MAX_BODY_BYTES, 1, MAX_BODY_BYTES));
        assert!(would_exceed(MAX_BODY_BYTES - 10, 11, MAX_BODY_BYTES));
        // Saturating add: a huge chunk can't wrap around to look small.
        assert!(would_exceed(1, usize::MAX, MAX_BODY_BYTES));
    }
}
