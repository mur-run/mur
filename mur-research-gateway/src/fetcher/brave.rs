//! Brave Search: the first-class search backend.
//!
//! Everything Brave-shaped, moved out of `fetcher.rs` verbatim. That file keeps
//! the keyless DuckDuckGo tier, the SSRF screen, and `fetch`.

use std::time::Duration;

use super::{FetchError, MAX_BODY_BYTES, SearchHit, build_client, screen_url_blocking};

/// Brave Search API web-search endpoint. First-class (real index, JSON, no
/// scraping) — used when a subscription token is configured.
const BRAVE_ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";

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
    let resp = client
        .get(screened.clone())
        .header("X-Subscription-Token", key)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| FetchError::Http(e.to_string()))?;
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
