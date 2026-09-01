//! Brave Search: the first-class search backend, and the 429 handling that
//! tells its two failure modes apart.
//!
//! Split out of `fetcher.rs` when the 429 classifier pushed that file past the
//! repo's 800-line limit. Everything Brave-shaped lives here; `fetcher.rs`
//! keeps the keyless DuckDuckGo tier, the SSRF screen, and `fetch`.

use std::time::Duration;

use super::{FetchError, MAX_BODY_BYTES, SearchHit, build_client, screen_url_blocking};

const BRAVE_ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";

/// Brave's free/credit tier allows **1 query per second**, and most plans
/// enforce that burst window AND a monthly quota at the same time. Both return
/// HTTP 429, and the two need opposite handling: a per-second trip clears in
/// about a second, a spent monthly quota clears next month. Treating them alike
/// is why a run once retried a request that could never succeed and then
/// reported "configure a Brave API key" to an operator who already had one.
///
/// `X-RateLimit-Reset` carries the seconds until each window frees up. We read
/// the SOONEST of them: if relief is close it is the burst window and worth
/// waiting for; if it is far away, no amount of backoff will help.
const BRAVE_RETRYABLE_RESET_SECS: u64 = 10;

/// Attempts against Brave when a 429 says the burst window will clear shortly.
/// Deliberately small: with several workers sharing one key the honest fix is
/// fewer concurrent searches, not a longer retry train.
const BRAVE_MAX_ATTEMPTS: u32 = 3;

/// What a Brave 429 actually meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BraveThrottle {
    /// Burst window; free again in `secs`.
    Burst { secs: u64 },
    /// Quota window; `secs` away, so retrying inside this run is pointless.
    Exhausted { secs: u64 },
    /// No usable `X-RateLimit-Reset`. Retried once as a burst, because a
    /// missing header is more likely a proxy stripping it than a spent quota.
    Unknown,
}

/// Classify a Brave 429 from its `X-RateLimit-Reset` header.
///
/// The header may carry one value per policy window (`"1, 1419704"`), so every
/// number is parsed and the minimum wins — that is when search can next work.
fn classify_brave_429(reset_header: Option<&str>) -> BraveThrottle {
    let Some(soonest) = reset_header.and_then(|h| {
        h.split(',')
            .filter_map(|part| part.trim().parse::<u64>().ok())
            .min()
    }) else {
        return BraveThrottle::Unknown;
    };
    if soonest <= BRAVE_RETRYABLE_RESET_SECS {
        BraveThrottle::Burst { secs: soonest }
    } else {
        BraveThrottle::Exhausted { secs: soonest }
    }
}

impl BraveThrottle {
    /// How this 429 reads to a human, so the fallback message says what
    /// actually happened instead of guessing.
    fn describe(self) -> String {
        match self {
            BraveThrottle::Burst { secs } => format!(
                "brave rate limit (1 query/sec on the free tier; window clears in {secs}s) —                  several workers are sharing one key"
            ),
            BraveThrottle::Exhausted { secs } => format!(
                "brave quota exhausted (next window in {}h) — retrying will not help;                  raise the plan or reduce searches",
                secs / 3600
            ),
            BraveThrottle::Unknown => "brave rate limit (429, no reset header)".to_string(),
        }
    }
}

/// Brave web search: GET the Brave API through the same proxy-honoring reqwest
/// path, authenticated with `X-Subscription-Token`. Screens the endpoint host
/// via the SSRF guard exactly like a fetch. Brave returns real URLs directly
/// (no `uddg` redirect to decode) as structured JSON.
pub(super) async fn search_brave(
    query: &str,
    limit: usize,
    key: &str,
    deny: &[String],
    timeout: Duration,
) -> Result<Vec<SearchHit>, FetchError> {
    let mut url = url::Url::parse(BRAVE_ENDPOINT).expect("static URL is valid");
    url.query_pairs_mut()
        .append_pair("q", query)
        .append_pair("count", &limit.to_string());
    let screened = screen_url_blocking(url.as_str(), deny)
        .await
        .map_err(FetchError::Guard)?;

    let client = build_client(timeout)?;
    let mut resp = None;
    for attempt in 0..BRAVE_MAX_ATTEMPTS {
        let r = client
            .get(screened.clone())
            .header("X-Subscription-Token", key)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| FetchError::Http(e.to_string()))?;
        if r.status().as_u16() != 429 {
            resp = Some(r);
            break;
        }
        // A 429 is two different failures wearing one status code. Wait only
        // for the one waiting can fix, and name the other instead of burning
        // attempts on it.
        let throttle = classify_brave_429(
            r.headers()
                .get("x-ratelimit-reset")
                .and_then(|v| v.to_str().ok()),
        );
        let wait = match throttle {
            BraveThrottle::Burst { secs } => secs.max(1),
            BraveThrottle::Unknown => 1,
            BraveThrottle::Exhausted { .. } => {
                return Err(FetchError::Http(throttle.describe()));
            }
        };
        if attempt + 1 == BRAVE_MAX_ATTEMPTS {
            return Err(FetchError::Http(throttle.describe()));
        }
        tokio::time::sleep(Duration::from_secs(wait)).await;
    }
    let resp = resp.expect("loop returns on every non-retry path");
    let status = resp.status();
    if !status.is_success() {
        return Err(FetchError::Http(format!(
            "brave api status {}",
            status.as_u16()
        )));
    }
    if let Some(len) = resp.content_length()
        && len > MAX_BODY_BYTES as u64
    {
        return Err(FetchError::TooLarge);
    }
    let body = resp
        .text()
        .await
        .map_err(|e| FetchError::Http(e.to_string()))?;
    parse_brave_hits(&body, limit).map_err(FetchError::Http)
}

/// Parse Brave's `web.results[]` JSON into hits. A missing `web` block (Brave
/// returns none when there are zero web results) is a valid empty result, not
/// an error; malformed JSON is an error so the caller falls back to DDG.
fn parse_brave_hits(json: &str, limit: usize) -> Result<Vec<SearchHit>, String> {
    #[derive(serde::Deserialize)]
    struct BraveResp {
        web: Option<BraveWeb>,
    }
    #[derive(serde::Deserialize)]
    struct BraveWeb {
        results: Vec<BraveResult>,
    }
    #[derive(serde::Deserialize)]
    struct BraveResult {
        title: String,
        url: String,
        #[serde(default)]
        description: String,
    }
    let resp: BraveResp = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(resp
        .web
        .map(|w| w.results)
        .unwrap_or_default()
        .into_iter()
        .take(limit)
        .map(|r| SearchHit {
            title: r.title,
            url: r.url,
            snippet: r.description,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 429 is two different failures wearing one status code. Waiting fixes
    /// exactly one of them; conflating them once produced an hour of retrying
    /// a request that could never succeed.
    #[test]
    fn a_burst_429_is_told_apart_from_a_spent_quota() {
        // 1 query/sec window: relief is immediate, so retry.
        assert_eq!(
            classify_brave_429(Some("1")),
            BraveThrottle::Burst { secs: 1 }
        );
        // Monthly window: no backoff inside this run can clear it.
        assert!(matches!(
            classify_brave_429(Some("1419704")),
            BraveThrottle::Exhausted { .. }
        ));
        // Both windows reported together — the SOONEST is when search can work.
        assert_eq!(
            classify_brave_429(Some("1, 1419704")),
            BraveThrottle::Burst { secs: 1 }
        );
        // A stripped header is likelier a proxy than a spent quota: retry once.
        assert_eq!(classify_brave_429(None), BraveThrottle::Unknown);
        assert_eq!(classify_brave_429(Some("garbage")), BraveThrottle::Unknown);
    }

    #[test]
    fn parse_brave_hits_extracts_results_and_respects_limit() {
        // Trimmed real Brave web-search JSON shape.
        let json = r#"{"web":{"results":[
            {"title":"First","url":"https://a.example","description":"snippet a"},
            {"title":"Second","url":"https://b.example","description":"snippet b"},
            {"title":"Third","url":"https://c.example"}
        ]}}"#;
        let hits = parse_brave_hits(json, 2).unwrap();
        assert_eq!(hits.len(), 2); // limit respected
        assert_eq!(hits[0].url, "https://a.example");
        assert_eq!(hits[0].snippet, "snippet a");
    }

    #[test]
    fn parse_brave_hits_missing_web_block_is_empty_not_error() {
        // Brave omits `web` when there are zero web results — valid, not a parse error.
        assert!(
            parse_brave_hits(r#"{"query":{"original":"x"}}"#, 8)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn parse_brave_hits_malformed_json_is_error() {
        // Malformed → Err so the dispatcher falls back to DDG.
        assert!(parse_brave_hits("not json", 8).is_err());
    }
}
