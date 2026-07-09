// mur-research-gateway/src/fetcher.rs
use crate::net_guard::{self, GuardReject};
use serde::Serialize;
use std::time::Duration;

// pub(crate) so browser.rs (tiers 2/3) enforces the SAME body cap on
// agent-browser stdout that tier-1 enforces on the HTTP body.
pub(crate) const MAX_BODY_BYTES: usize = 5 * 1024 * 1024; // ponytail: 5MB cap; config if a real doc exceeds it

/// Browser-like UA — DuckDuckGo's html endpoint returns HTTP 202 (a challenge
/// interstitial, no results) to requests without one. Verified live 2026-07-09.
const SEARCH_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/120.0 Safari/537.36";

/// DuckDuckGo's server-rendered (no-JS) HTML search endpoint.
const DDG_HTML_ENDPOINT: &str = "https://html.duckduckgo.com/html/";

#[derive(Debug, Serialize)]
pub struct FetchResult {
    pub url: String,
    pub status: u16,
    pub title: Option<String>,
    pub text: String,
    pub tier: u8,
}

#[derive(Debug, Serialize)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
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
    let client = build_client(timeout)?;
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

/// Shared tier-1 reqwest client: env-proxy honoring (so the runtime's
/// `HTTPS_PROXY` reaches it — the G1 path), per-request timeout, a
/// browser-like UA (some hosts, incl. DDG's html endpoint, 202/deny without
/// one), and no auto-redirect (each hop is re-screened by the caller).
fn build_client(timeout: Duration) -> Result<reqwest::Client, FetchError> {
    reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(SEARCH_USER_AGENT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| FetchError::Http(e.to_string()))
}

/// Tier-1 web search: GET DuckDuckGo's html endpoint through the same
/// proxy-honoring reqwest path `fetch_tier1` uses (works under the kernel
/// sandbox; agent-browser does not — G2), then parse result anchors. Screens
/// the endpoint host via the SSRF guard exactly like a fetch.
pub async fn search_tier1(
    query: &str,
    limit: usize,
    deny: &[String],
    timeout: Duration,
) -> Result<Vec<SearchHit>, FetchError> {
    let mut search_url = url::Url::parse(DDG_HTML_ENDPOINT).expect("static URL is valid");
    search_url.query_pairs_mut().append_pair("q", query);

    let screened = screen_url_blocking(search_url.as_str(), deny)
        .await
        .map_err(FetchError::Guard)?;

    let client = build_client(timeout)?;
    let mut resp = client
        .get(screened.clone())
        .send()
        .await
        .map_err(|e| FetchError::Http(e.to_string()))?;
    if let Some(len) = resp.content_length()
        && len > MAX_BODY_BYTES as u64
    {
        return Err(FetchError::TooLarge);
    }
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
    Ok(parse_ddg_hits(&body, limit))
}

/// Parse DuckDuckGo html-endpoint results into hits. Keys on `result__a`
/// anchors whose href wraps the real URL in the `uddg` query param; the
/// snippet is the nearest following `result__snippet` text. Deliberately
/// minimal — DDG's markup is not a contract MUR controls (spec §11), so a
/// focused scan beats pulling in an HTML-parser dependency.
fn parse_ddg_hits(html: &str, limit: usize) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    // Split on the result-anchor class marker; each piece after the first
    // starts inside a result__a tag.
    let mut segments = html.split("class=\"result__a\"");
    let _ = segments.next(); // preamble before the first anchor
    for seg in segments {
        if hits.len() >= limit {
            break;
        }
        let Some(href) = attr_after(seg, "href=\"") else {
            continue;
        };
        let Some(url) = decode_uddg(&href) else {
            continue;
        };
        let title = strip_tags(inner_text_after_tag(seg));
        // Snippet: nearest following result__snippet inner text, if any before
        // the next result anchor (segments already end at the next anchor).
        let snippet = seg
            .split_once("class=\"result__snippet\"")
            .map(|(_, rest)| strip_tags(inner_text_after_tag(rest)))
            .unwrap_or_default();
        hits.push(SearchHit {
            title,
            url,
            snippet,
        });
    }
    hits
}

/// Value of the first `needle`-prefixed attribute in `s` (up to the next `"`).
fn attr_after(s: &str, needle: &str) -> Option<String> {
    let start = s.find(needle)? + needle.len();
    let rest = &s[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Text between the first `>` and the anchor's closing `</a>` (an element's
/// full inner-HTML run, which may still contain nested inline tags like
/// `<b>` — `strip_tags` handles those). Bounding on `</a>` rather than the
/// next `<` matters: DDG titles wrap matched query terms in `<b>…</b>`, and
/// stopping at the first `<` would truncate the title before that markup.
/// HTML-entity-naive (DDG titles are plain).
fn inner_text_after_tag(s: &str) -> String {
    let Some(gt) = s.find('>') else {
        return String::new();
    };
    let rest = &s[gt + 1..];
    let end = rest.find("</a>").unwrap_or(rest.len());
    rest[..end].to_string()
}

/// Strip any remaining inline tags (e.g. `<b>` in a title) and collapse
/// whitespace. Reuses the same tag-stripping idea as `html_to_text`.
fn strip_tags(s: String) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Decode DDG's redirect href `//duckduckgo.com/l/?uddg=<percent-encoded real
/// url>&rut=…` into the real URL. Also accepts a bare absolute URL (defensive).
fn decode_uddg(href: &str) -> Option<String> {
    let abs = if let Some(stripped) = href.strip_prefix("//") {
        format!("https://{stripped}")
    } else {
        href.to_string()
    };
    let parsed = url::Url::parse(&abs).ok()?;
    if let Some((_, v)) = parsed.query_pairs().find(|(k, _)| k == "uddg") {
        return Some(v.into_owned());
    }
    // No uddg param → treat an http(s) href as already-real, else skip.
    (abs.starts_with("http://") || abs.starts_with("https://")).then_some(abs)
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

    #[test]
    fn parses_ddg_result_anchors_and_decodes_uddg() {
        // Trimmed real DDG html endpoint shape (2026-07-09): result links are
        // `result__a` anchors whose href wraps the real URL in the `uddg`
        // query param; snippet is a following `result__snippet` element.
        let html = r#"
        <div class="result">
          <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa&amp;rut=x">First <b>Title</b></a>
          <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa">Snippet one text.</a>
        </div>
        <div class="result">
          <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust-lang.org%2F&amp;rut=y">Second</a>
        </div>"#;
        let hits = parse_ddg_hits(html, 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://example.com/a");
        assert_eq!(hits[0].title, "First Title"); // tags stripped
        assert_eq!(hits[0].snippet, "Snippet one text.");
        assert_eq!(hits[1].url, "https://rust-lang.org/");
        assert_eq!(hits[1].snippet, ""); // no snippet element → empty, tolerated
    }

    #[test]
    fn parse_ddg_hits_respects_limit() {
        let block = |u: &str| {
            format!(
                r#"<a class="result__a" href="//duckduckgo.com/l/?uddg={u}">t</a>"#,
                u = u
            )
        };
        let html = format!(
            "{}{}{}",
            block("https%3A%2F%2Fa.test%2F"),
            block("https%3A%2F%2Fb.test%2F"),
            block("https%3A%2F%2Fc.test%2F")
        );
        assert_eq!(parse_ddg_hits(&html, 2).len(), 2);
    }
}
