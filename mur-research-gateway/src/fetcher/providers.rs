//! Tavily, SerpApi and Firecrawl — the keyed search backends added alongside
//! Brave.
//!
//! Brave keeps its own module (`brave.rs`) because its 429 handling is a real
//! algorithm; these three are each a request, a status check and a parse, so
//! they share one file rather than three near-empty ones.
//!
//! Each provider authenticates differently, and getting that wrong fails as a
//! 401 that looks exactly like a bad key. The three shapes below were verified
//! live against each API on 2026-07-13 by sending a deliberately invalid key
//! and reading the rejection:
//!
//! | provider  | method | auth                              | rejection body |
//! |-----------|--------|-----------------------------------|----------------|
//! | Tavily    | POST   | `Authorization: Bearer <key>`     | `{"detail":{"error":"Unauthorized: missing or invalid API key."}}` |
//! | SerpApi   | GET    | `api_key=<key>` query parameter   | `{"error":"Invalid API key. …"}` |
//! | Firecrawl | POST   | `Authorization: Bearer <key>`     | `{"success":false,"error":"Unauthorized: Invalid token"}` |
//!
//! The SUCCESS shapes are each provider's documented response envelope. They
//! are parsed leniently — a missing results array is an empty result, not an
//! error — so a provider adding fields never breaks search, while malformed
//! JSON IS an error so the caller falls back to the next provider.

use std::time::Duration;

use super::{FetchError, MAX_BODY_BYTES, SearchHit, build_client, screen_url_blocking};

const TAVILY_ENDPOINT: &str = "https://api.tavily.com/search";
const SERPAPI_ENDPOINT: &str = "https://serpapi.com/search.json";
const FIRECRAWL_ENDPOINT: &str = "https://api.firecrawl.dev/v1/search";

/// Read the body of an already-screened response, enforcing the shared body
/// cap. Every provider below needs exactly this, and the cap must not be one
/// backend's private decision.
async fn read_capped_body(resp: reqwest::Response, provider: &str) -> Result<String, FetchError> {
    let status = resp.status();
    if !status.is_success() {
        // 401/403 is overwhelmingly "the key is wrong", and saying so saves an
        // operator from debugging their network instead of their key.
        let hint = match status.as_u16() {
            401 | 403 => " (check the API key)",
            429 => " (rate limited or quota exhausted)",
            _ => "",
        };
        return Err(FetchError::Http(format!(
            "{provider} api status {}{hint}",
            status.as_u16()
        )));
    }
    if let Some(len) = resp.content_length()
        && len > MAX_BODY_BYTES as u64
    {
        return Err(FetchError::TooLarge);
    }
    resp.text()
        .await
        .map_err(|e| FetchError::Http(e.to_string()))
}

/// Tavily: POST JSON, bearer auth. Built for LLM agents, so its `content`
/// field is already an extract rather than an HTML snippet.
pub(super) async fn search_tavily(
    query: &str,
    limit: usize,
    key: &str,
    deny: &[String],
    timeout: Duration,
) -> Result<Vec<SearchHit>, FetchError> {
    let screened = screen_url_blocking(TAVILY_ENDPOINT, deny)
        .await
        .map_err(FetchError::Guard)?;
    let client = build_client(timeout)?;
    let resp = client
        .post(screened)
        .bearer_auth(key)
        .json(&serde_json::json!({ "query": query, "max_results": limit }))
        .send()
        .await
        .map_err(|e| FetchError::Http(e.to_string()))?;
    let body = read_capped_body(resp, "tavily").await?;
    parse_tavily_hits(&body, limit).map_err(FetchError::Http)
}

fn parse_tavily_hits(json: &str, limit: usize) -> Result<Vec<SearchHit>, String> {
    #[derive(serde::Deserialize)]
    struct Resp {
        #[serde(default)]
        results: Vec<Item>,
    }
    #[derive(serde::Deserialize)]
    struct Item {
        #[serde(default)]
        title: String,
        url: String,
        #[serde(default)]
        content: String,
    }
    let resp: Resp = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(resp
        .results
        .into_iter()
        .take(limit)
        .map(|r| SearchHit {
            title: r.title,
            url: r.url,
            snippet: r.content,
        })
        .collect())
}

/// SerpApi: GET with the key as a query parameter — the one backend here that
/// puts its credential in the URL. That URL therefore must never be logged;
/// only the endpoint constant above is safe to print.
pub(super) async fn search_serpapi(
    query: &str,
    limit: usize,
    key: &str,
    deny: &[String],
    timeout: Duration,
) -> Result<Vec<SearchHit>, FetchError> {
    let mut url = url::Url::parse(SERPAPI_ENDPOINT).expect("static URL is valid");
    url.query_pairs_mut()
        .append_pair("q", query)
        .append_pair("num", &limit.to_string())
        .append_pair("api_key", key);
    // Screen the key-free endpoint: the guard's reject message is user-facing
    // and must not be able to carry the credential.
    screen_url_blocking(SERPAPI_ENDPOINT, deny)
        .await
        .map_err(FetchError::Guard)?;
    let client = build_client(timeout)?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| FetchError::Http(e.to_string()))?;
    let body = read_capped_body(resp, "serpapi").await?;
    parse_serpapi_hits(&body, limit).map_err(FetchError::Http)
}

fn parse_serpapi_hits(json: &str, limit: usize) -> Result<Vec<SearchHit>, String> {
    #[derive(serde::Deserialize)]
    struct Resp {
        #[serde(default)]
        organic_results: Vec<Item>,
        /// SerpApi reports a bad key as HTTP 200 with an `error` field, so a
        /// success status is not proof of a search. Surfaced as an error so
        /// the dispatcher falls through to the next provider.
        #[serde(default)]
        error: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct Item {
        #[serde(default)]
        title: String,
        link: String,
        #[serde(default)]
        snippet: String,
    }
    let resp: Resp = serde_json::from_str(json).map_err(|e| e.to_string())?;
    if let Some(e) = resp.error {
        return Err(format!("serpapi error: {e}"));
    }
    Ok(resp
        .organic_results
        .into_iter()
        .take(limit)
        .map(|r| SearchHit {
            title: r.title,
            url: r.link,
            snippet: r.snippet,
        })
        .collect())
}

/// Firecrawl: POST JSON, bearer auth, results under `data`.
pub(super) async fn search_firecrawl(
    query: &str,
    limit: usize,
    key: &str,
    deny: &[String],
    timeout: Duration,
) -> Result<Vec<SearchHit>, FetchError> {
    let screened = screen_url_blocking(FIRECRAWL_ENDPOINT, deny)
        .await
        .map_err(FetchError::Guard)?;
    let client = build_client(timeout)?;
    let resp = client
        .post(screened)
        .bearer_auth(key)
        .json(&serde_json::json!({ "query": query, "limit": limit }))
        .send()
        .await
        .map_err(|e| FetchError::Http(e.to_string()))?;
    let body = read_capped_body(resp, "firecrawl").await?;
    parse_firecrawl_hits(&body, limit).map_err(FetchError::Http)
}

fn parse_firecrawl_hits(json: &str, limit: usize) -> Result<Vec<SearchHit>, String> {
    #[derive(serde::Deserialize)]
    struct Resp {
        #[serde(default)]
        data: Vec<Item>,
        /// Firecrawl signals failure in-band (`{"success":false,"error":…}`).
        #[serde(default)]
        success: Option<bool>,
        #[serde(default)]
        error: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct Item {
        #[serde(default)]
        title: String,
        url: String,
        #[serde(default)]
        description: String,
    }
    let resp: Resp = serde_json::from_str(json).map_err(|e| e.to_string())?;
    if resp.success == Some(false) {
        return Err(format!(
            "firecrawl error: {}",
            resp.error.as_deref().unwrap_or("unspecified")
        ));
    }
    Ok(resp
        .data
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
    fn tavily_parses_results_and_respects_limit() {
        let json = r#"{"results":[
            {"title":"First","url":"https://a.example","content":"extract a"},
            {"title":"Second","url":"https://b.example","content":"extract b"},
            {"title":"Third","url":"https://c.example"}
        ]}"#;
        let hits = parse_tavily_hits(json, 2).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://a.example");
        assert_eq!(hits[0].snippet, "extract a");
    }

    #[test]
    fn tavily_missing_results_is_empty_not_error() {
        assert!(parse_tavily_hits(r#"{"query":"x"}"#, 8).unwrap().is_empty());
    }

    #[test]
    fn serpapi_parses_organic_results() {
        let json = r#"{"organic_results":[
            {"title":"First","link":"https://a.example","snippet":"snip a"},
            {"title":"Second","link":"https://b.example","snippet":"snip b"}
        ]}"#;
        let hits = parse_serpapi_hits(json, 8).unwrap();
        assert_eq!(hits.len(), 2);
        // SerpApi calls it `link`, not `url` — mapping it wrong yields empty
        // URLs that fail much later, in the worker.
        assert_eq!(hits[0].url, "https://a.example");
    }

    /// Verified live 2026-07-13: an invalid SerpApi key comes back as HTTP 200
    /// carrying `{"error":"Invalid API key. …"}`. A success status is not
    /// proof of a search, so the body has to be checked too.
    #[test]
    fn serpapi_in_band_error_is_an_error_despite_http_200() {
        let json = r#"{"error":"Invalid API key. Your API key should be here: https://serpapi.com/manage-api-key"}"#;
        let err = parse_serpapi_hits(json, 8).unwrap_err();
        assert!(err.contains("Invalid API key"), "{err}");
    }

    #[test]
    fn firecrawl_parses_data_array() {
        let json = r#"{"success":true,"data":[
            {"title":"First","url":"https://a.example","description":"desc a"}
        ]}"#;
        let hits = parse_firecrawl_hits(json, 8).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].snippet, "desc a");
    }

    /// Verified live 2026-07-13: `{"success":false,"error":"Unauthorized: Invalid token"}`.
    #[test]
    fn firecrawl_success_false_is_an_error() {
        let json = r#"{"success":false,"error":"Unauthorized: Invalid token"}"#;
        let err = parse_firecrawl_hits(json, 8).unwrap_err();
        assert!(err.contains("Invalid token"), "{err}");
    }

    #[test]
    fn malformed_json_is_an_error_for_every_provider() {
        // Malformed → Err so the dispatcher falls through to the next backend.
        assert!(parse_tavily_hits("not json", 8).is_err());
        assert!(parse_serpapi_hits("not json", 8).is_err());
        assert!(parse_firecrawl_hits("not json", 8).is_err());
    }
}
