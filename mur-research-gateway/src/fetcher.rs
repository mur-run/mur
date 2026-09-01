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

/// Max `search` attempts against DuckDuckGo before giving up. Under N concurrent
/// research workers hitting DDG's html endpoint from one IP, DDG rate-limits
/// with an HTTP 202 challenge page (no results); a short backoff usually clears
/// it. 3 attempts = the original + 2 retries.
const SEARCH_MAX_ATTEMPTS: u32 = 3;

/// Base backoff before the first retry; doubles each subsequent attempt.
const SEARCH_BASE_BACKOFF_MS: u64 = 400;

/// HTTP 202 = DuckDuckGo's anti-bot challenge / rate-limit interstitial (no
/// results). The only status we retry on — a 200 with genuinely no hits is
/// accepted as-is.
const DDG_CHALLENGE_STATUS: u16 = 202;

/// Backoff before `attempt` (1-based retry index) of a search: exponential
/// (`base * 2^(attempt-1)`) plus a query-derived jitter of up to `base`. The
/// jitter staggers concurrent workers — each has a distinct sub-question, so a
/// query-seeded jitter spreads their retries across the window instead of
/// synchronizing another burst. Pure → unit-testable, no clock/RNG dependency.
fn search_backoff(attempt: u32, query: &str) -> Duration {
    let base = SEARCH_BASE_BACKOFF_MS;
    let exp = base.saturating_mul(1u64 << (attempt.saturating_sub(1)).min(16));
    // FNV-1a of the query → jitter in [0, base).
    let mut h: u64 = 1469598103934665603;
    for b in query.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    let jitter = h % base.max(1);
    Duration::from_millis(exp.saturating_add(jitter))
}

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

/// Cap `text` to at most `max_chars` characters for the tool result the worker
/// feeds into its LLM context — a full page can otherwise overflow the model
/// (deep-research turns died with anthropic 400 "prompt is too long"). Counts
/// CHARACTERS (not bytes) and cuts on a codepoint boundary. `max_chars == 0`
/// disables the cap (operator opt-out). On truncation, appends a marker naming
/// how many chars were dropped so the model knows the text was cut.
pub(crate) fn cap_text(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return text.to_string();
    }
    // char_indices()nth gives the byte offset of the (max_chars)-th char, i.e.
    // a guaranteed codepoint boundary; None means the text is already shorter.
    match text.char_indices().nth(max_chars) {
        None => text.to_string(),
        Some((byte_idx, _)) => {
            let dropped = text.chars().count() - max_chars;
            format!("{}\n…[truncated {dropped} chars]", &text[..byte_idx])
        }
    }
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

mod brave;

/// Pluggable web search: `brave_key.is_some()` → Brave's first-class API;
/// `None` → scrape DuckDuckGo's HTML endpoint (keyless, zero-config). If Brave
/// is configured but errors (bad key, quota, transport), degrade to DDG rather
/// than fail the search — a misconfigured key must never black out research.
pub async fn search(
    query: &str,
    limit: usize,
    brave_key: Option<&str>,
    deny: &[String],
    timeout: Duration,
    endpoint: &str,
) -> Result<Vec<SearchHit>, FetchError> {
    match brave_key {
        Some(key) => match brave::search_brave(query, limit, key, deny, timeout).await {
            Ok(hits) => Ok(hits),
            Err(e) => {
                tracing::warn!(
                    target: "research_gateway",
                    "brave search failed ({}), falling back to DuckDuckGo",
                    fetch_err_brief(&e)
                );
                search_tier1(query, limit, deny, timeout, endpoint).await
            }
        },
        None => search_tier1(query, limit, deny, timeout, endpoint).await,
    }
}

/// Short human label for a `FetchError` used in the Brave→DDG fallback log.
fn fetch_err_brief(e: &FetchError) -> String {
    match e {
        FetchError::Guard(_) => "ssrf-guard".to_string(),
        FetchError::Http(m) => m.clone(),
        FetchError::TooLarge => "response too large".to_string(),
    }
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
    endpoint: &str,
) -> Result<Vec<SearchHit>, FetchError> {
    // Operator-supplied (config/env), so a bad value must surface as an error
    // the worker can read — never a panic that takes the gateway down.
    let mut search_url = url::Url::parse(endpoint).map_err(|e| {
        FetchError::Http(format!(
            "search endpoint is not a valid URL ({e}): {endpoint:?} — fix \
             research_gateway.search_endpoint in ~/.mur/config.yaml or \
             MUR_RESEARCH_SEARCH_ENDPOINT"
        ))
    })?;
    search_url.query_pairs_mut().append_pair("q", query);

    let screened = screen_url_blocking(search_url.as_str(), deny)
        .await
        .map_err(FetchError::Guard)?;

    // Retry the DuckDuckGo 202 challenge (rate-limit under concurrent workers)
    // with exponential backoff + query-seeded jitter. A transport error is also
    // retried; a 200 (even with no hits) is accepted as the final answer.
    let mut last_err: Option<FetchError> = None;
    for attempt in 0..SEARCH_MAX_ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(search_backoff(attempt, query)).await;
        }
        match search_attempt(&screened, limit, timeout).await {
            Ok((DDG_CHALLENGE_STATUS, _)) => continue, // challenged → back off + retry
            Ok((_, hits)) => return Ok(hits),
            Err(e) => last_err = Some(e),
        }
    }
    // Exhausted retries: surface the last transport error if any. A challenge
    // that never cleared is reported as an explicit, actionable error — NOT an
    // empty hit list. An empty Ok is indistinguishable from "no results", so
    // workers kept re-querying a permanently blocked endpoint (live: two full
    // deep-research runs burned 9–15 queries each against a persistent
    // IP-level 202 block, 2026-07-14/15). Tool errors surface to the worker as
    // a readable message (server.rs -32001), which workers already handle by
    // pivoting to direct `fetch` — degrading with a reason beats degrading
    // silently.
    match last_err {
        Some(e) => Err(e),
        None => Err(search_blocked_error()),
    }
}

/// The explicit error returned when DDG's anti-bot challenge never clears.
/// Names both the worker-side workaround (direct `fetch`) and the operator
/// fix (Brave key) so the message is actionable at both levels.
fn search_blocked_error() -> FetchError {
    FetchError::Http(format!(
        "DuckDuckGo returned its anti-bot challenge (HTTP {DDG_CHALLENGE_STATUS}) on all \
         {SEARCH_MAX_ATTEMPTS} attempts — this host's IP appears persistently blocked, \
         retrying will not help. Use direct `fetch` of known URLs instead. Operator fix: \
         configure a free Brave Search API key (research_gateway.brave_api_key — or \
         brave_api_key_ref, e.g. keychain:mur/brave — in \
         ~/.mur/config.yaml, or MUR_RESEARCH_BRAVE_KEY) to switch search off DuckDuckGo."
    ))
}

/// One search request: GET the (already-screened) DDG url, enforce the body
/// cap, and parse. Returns the HTTP status alongside the hits so the caller can
/// distinguish a 202 challenge (retry) from a real 200 result (accept).
async fn search_attempt(
    screened: &url::Url,
    limit: usize,
    timeout: Duration,
) -> Result<(u16, Vec<SearchHit>), FetchError> {
    let client = build_client(timeout)?;
    let mut resp = client
        .get(screened.clone())
        .send()
        .await
        .map_err(|e| FetchError::Http(e.to_string()))?;
    let status = resp.status().as_u16();
    if status == DDG_CHALLENGE_STATUS {
        return Ok((status, Vec::new()));
    }
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
    Ok((status, parse_ddg_hits(&body, limit)))
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
    async fn search_tier1_rejects_a_malformed_endpoint_instead_of_panicking() {
        // The endpoint is operator input (config.yaml / env), so a bad value
        // must come back as an error the worker can read — the old code
        // `.expect("static URL is valid")`d it, which was only safe while the
        // endpoint was a hardcoded const. No network is touched: the URL fails
        // to parse before any request is built.
        let err = search_tier1("q", 3, &[], Duration::from_secs(1), "not a url")
            .await
            .expect_err("a malformed endpoint must be an error, not a panic");
        let FetchError::Http(msg) = err else {
            panic!("malformed endpoint must be FetchError::Http");
        };
        // Name both operator config paths so the message stays actionable.
        assert!(msg.contains("search_endpoint"), "message was: {msg}");
        assert!(
            msg.contains("MUR_RESEARCH_SEARCH_ENDPOINT"),
            "message was: {msg}"
        );
    }

    #[test]
    fn search_blocked_error_names_workaround_and_operator_fix() {
        // Pin the actionable pointers so they can't silently rot: the worker
        // pivot (`fetch`) and both operator config paths for the Brave key.
        let FetchError::Http(msg) = search_blocked_error() else {
            panic!("blocked error must be FetchError::Http");
        };
        assert!(msg.contains("fetch"));
        assert!(msg.contains("MUR_RESEARCH_BRAVE_KEY"));
        assert!(msg.contains("research_gateway.brave_api_key"));
        assert!(msg.contains("202"));
    }

    #[test]
    fn search_backoff_grows_exponentially_with_bounded_jitter() {
        let base = SEARCH_BASE_BACKOFF_MS;
        // Jitter is in [0, base); so attempt N is in [base*2^(N-1), base*2^(N-1) + base).
        for (attempt, floor) in [(1u32, base), (2, base * 2), (3, base * 4)] {
            let d = search_backoff(attempt, "some query").as_millis() as u64;
            assert!(
                d >= floor && d < floor + base,
                "attempt {attempt}: {d}ms not in [{floor}, {})",
                floor + base
            );
        }
    }

    #[test]
    fn search_backoff_jitter_differs_by_query() {
        // Distinct sub-questions get distinct jitter → concurrent workers stagger
        // instead of re-bursting in sync.
        let a = search_backoff(1, "privacy architecture of ollama");
        let b = search_backoff(1, "extensibility of lm studio");
        assert_ne!(a, b, "different queries must produce different backoff");
        // Same query is deterministic (no clock/RNG).
        assert_eq!(a, search_backoff(1, "privacy architecture of ollama"));
    }

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

    #[test]
    fn cap_text_under_limit_is_unchanged() {
        assert_eq!(cap_text("hello world", 50_000), "hello world");
    }

    #[test]
    fn cap_text_zero_means_no_cap() {
        let big = "x".repeat(10_000);
        assert_eq!(cap_text(&big, 0), big);
    }

    #[test]
    fn cap_text_truncates_with_marker_on_char_boundary() {
        // 10 multibyte chars (é = 2 bytes each); cap at 4 chars.
        let s = "é".repeat(10);
        let out = cap_text(&s, 4);
        assert!(out.starts_with(&"é".repeat(4)));
        assert!(out.contains("[truncated 6 chars]"));
        // Never split a codepoint: the kept prefix is valid UTF-8 of 4 'é's.
        assert_eq!(out.chars().take_while(|&c| c == 'é').count(), 4);
    }
}
