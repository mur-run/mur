//! Notion workspace adapter.
//!
//! Auth: OAuth 2.0 + PKCE (public mur integration) — implemented in Task 5.
//! For P1.4 Step 1 we focus on the PAT (Internal Integration Token) path
//! which lets users connect immediately by passing a token they create in
//! https://www.notion.so/my-integrations.

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use governor::{Quota, RateLimiter};
use reqwest::Client;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;

use crate::sources::KnowledgeSource;
use crate::sources::chunker::notion_blocks;
use crate::sources::instance::SourceInstance;
use crate::sources::kind::SourceKind;
use crate::sources::types::{Chunk, DocRef, Document, DocumentBody, SyncCursor};

const NOTION_API_BASE: &str = "https://api.notion.com/v1";
const NOTION_VERSION: &str = "2022-06-28";
const RATE_LIMIT_PER_SEC: u32 = 3;
const CHUNK_MAX_CHARS: usize = 6000;

pub struct NotionAdapter {
    id: String,
    client: Client,
    token: String,
    #[allow(dead_code)] // reserved for workspace-scoped API calls (P1.5)
    workspace_id: Option<String>,
    weight: f32,
    limiter: Arc<
        RateLimiter<
            governor::state::NotKeyed,
            governor::state::InMemoryState,
            governor::clock::DefaultClock,
        >,
    >,
}

impl NotionAdapter {
    pub fn from_instance(instance: &SourceInstance, token: String) -> Result<Self> {
        if instance.type_name != "notion" {
            bail!("expected type_name 'notion', got '{}'", instance.type_name);
        }
        let workspace_id = instance
            .scope
            .get("workspace_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("build reqwest client")?;
        let limiter = Arc::new(RateLimiter::direct(Quota::per_second(
            std::num::NonZeroU32::new(RATE_LIMIT_PER_SEC).unwrap(),
        )));
        Ok(Self {
            id: instance.id.clone(),
            client,
            token,
            workspace_id,
            weight: instance.weight,
            limiter,
        })
    }

    async fn rl(&self) {
        self.limiter.until_ready().await;
    }
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    results: Vec<SearchResult>,
    next_cursor: Option<String>,
    has_more: bool,
}

#[derive(Debug, Deserialize)]
struct SearchResult {
    id: String,
    last_edited_time: Option<String>,
    archived: Option<bool>,
    properties: Option<serde_json::Value>,
    #[allow(dead_code)] // reserved for source-link metadata (P1.5)
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BlocksResponse {
    results: Vec<serde_json::Value>,
    next_cursor: Option<String>,
    has_more: bool,
}

#[async_trait]
impl KnowledgeSource for NotionAdapter {
    fn id(&self) -> &str {
        &self.id
    }
    fn kind(&self) -> SourceKind {
        SourceKind::PullIndex
    }
    fn weight(&self) -> f32 {
        self.weight
    }

    async fn list_documents(
        &self,
        cursor: Option<SyncCursor>,
    ) -> Result<(Vec<DocRef>, SyncCursor)> {
        let threshold: Option<DateTime<Utc>> = cursor.and_then(|c| {
            if c.is_empty() {
                None
            } else {
                DateTime::parse_from_rfc3339(&c.0)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))
            }
        });

        let mut docs: Vec<DocRef> = Vec::new();
        let mut max_ts: Option<DateTime<Utc>> = None;
        let mut start_cursor: Option<String> = None;

        loop {
            self.rl().await;
            let mut body = serde_json::json!({
                "filter": {"value": "page", "property": "object"},
                "page_size": 100,
            });
            if let Some(c) = &start_cursor {
                body["start_cursor"] = serde_json::Value::String(c.clone());
            }
            let resp = self
                .client
                .post(format!("{NOTION_API_BASE}/search"))
                .bearer_auth(&self.token)
                .header("Notion-Version", NOTION_VERSION)
                .json(&body)
                .send()
                .await
                .context("notion search request")?;
            let resp = retry_or_error(resp).await?;
            let s: SearchResponse = resp.json().await.context("decode notion search response")?;

            for r in s.results {
                if r.archived.unwrap_or(false) {
                    continue;
                }
                let updated_at = r
                    .last_edited_time
                    .as_deref()
                    .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(Utc::now);

                if let Some(t) = threshold
                    && updated_at <= t
                {
                    continue;
                }
                if max_ts.is_none() || max_ts.is_some_and(|m| updated_at > m) {
                    max_ts = Some(updated_at);
                }

                let title = title_from_properties(r.properties.as_ref());
                docs.push(DocRef {
                    external_id: r.id,
                    title,
                    updated_at,
                });
            }

            if s.has_more {
                start_cursor = s.next_cursor;
                if start_cursor.is_none() {
                    break;
                }
            } else {
                break;
            }
        }

        let cursor_out = match max_ts {
            Some(t) => SyncCursor(t.to_rfc3339()),
            None => SyncCursor(threshold.map(|t| t.to_rfc3339()).unwrap_or_default()),
        };
        Ok((docs, cursor_out))
    }

    async fn fetch(&self, doc_ref: &DocRef) -> Result<Document> {
        // Walk paginated /blocks/{id}/children
        let mut all_blocks: Vec<serde_json::Value> = Vec::new();
        let mut start_cursor: Option<String> = None;
        loop {
            self.rl().await;
            let mut url = format!(
                "{NOTION_API_BASE}/blocks/{}/children?page_size=100",
                doc_ref.external_id
            );
            if let Some(c) = &start_cursor {
                url.push_str(&format!("&start_cursor={c}"));
            }
            let resp = self
                .client
                .get(&url)
                .bearer_auth(&self.token)
                .header("Notion-Version", NOTION_VERSION)
                .send()
                .await
                .context("notion blocks request")?;
            let resp = retry_or_error(resp).await?;
            let b: BlocksResponse = resp.json().await.context("decode notion blocks response")?;
            all_blocks.extend(b.results);

            if b.has_more {
                start_cursor = b.next_cursor;
                if start_cursor.is_none() {
                    break;
                }
            } else {
                break;
            }
        }

        let title = doc_ref
            .title
            .clone()
            .unwrap_or_else(|| doc_ref.external_id.clone());
        let url = format!(
            "https://www.notion.so/{}",
            doc_ref.external_id.replace('-', "")
        );

        Ok(Document {
            source_id: self.id.clone(),
            external_id: doc_ref.external_id.clone(),
            title,
            body: DocumentBody::NotionBlocks(serde_json::Value::Array(all_blocks)),
            url: Some(url),
            updated_at: doc_ref.updated_at,
            tags: vec![],
            metadata: serde_json::Value::Null,
        })
    }

    fn chunk(&self, doc: &Document) -> Result<Vec<Chunk>> {
        let blocks: &[serde_json::Value] = match &doc.body {
            DocumentBody::NotionBlocks(serde_json::Value::Array(arr)) => arr,
            _ => bail!("notion adapter expects NotionBlocks(Array) body"),
        };
        let md = notion_blocks::blocks_to_markdown(blocks);
        let raw =
            crate::sources::chunker::markdown::chunk_markdown(&doc.title, &md, CHUNK_MAX_CHARS);
        let mut out = Vec::with_capacity(raw.len());
        for (i, c) in raw.into_iter().enumerate() {
            out.push(Chunk::new(
                doc.source_id.clone(),
                doc.external_id.clone(),
                i,
                c.text,
                c.heading_path,
                c.char_range,
                doc.updated_at,
            ));
        }
        Ok(out)
    }
}

fn title_from_properties(props: Option<&serde_json::Value>) -> Option<String> {
    let props = props?;
    let obj = props.as_object()?;
    for (_, v) in obj {
        let kind = v.get("type")?.as_str()?;
        if kind == "title" {
            let arr = v.get("title")?.as_array()?;
            let mut s = String::new();
            for span in arr {
                if let Some(t) = span.get("plain_text").and_then(|x| x.as_str()) {
                    s.push_str(t);
                }
            }
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    None
}

async fn retry_or_error(resp: reqwest::Response) -> Result<reqwest::Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let wait_secs = resp
            .headers()
            .get("Retry-After")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(1);
        tokio::time::sleep(Duration::from_secs(wait_secs)).await;
        bail!("notion rate-limited (429); retry after {wait_secs}s");
    }
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    bail!("notion api error {status}: {text}")
}

// ---------- OAuth (PKCE) ----------

const NOTION_OAUTH_AUTHORIZE: &str = "https://api.notion.com/v1/oauth/authorize";
const NOTION_OAUTH_TOKEN: &str = "https://api.notion.com/v1/oauth/token";
const NOTION_CLIENT_ID: &str = match option_env!("MUR_NOTION_CLIENT_ID") {
    Some(v) => v,
    None => "FILL_ME_IN",
};

/// Outcome of the OAuth flow.
pub struct OAuthResult {
    pub access_token: String,
    pub workspace_id: Option<String>,
    pub workspace_name: Option<String>,
}

/// Run the OAuth dance. Spawns an axum server on a random port, opens a browser,
/// waits for the callback, exchanges the code, returns the token.
///
/// Notion's OAuth uses confidential-client mode by default — but this works
/// for self-hosted PKCE too with `client_secret = ""`. If you control the
/// integration (recommended for personal mur builds), use a "public" type.
#[cfg(feature = "server")]
pub async fn run_oauth_flow() -> Result<OAuthResult> {
    use axum::{Router, extract::Query, response::Html, routing::get};
    use oauth2::{
        AuthUrl, AuthorizationCode, ClientId, CsrfToken, PkceCodeChallenge, RedirectUrl,
        TokenResponse, TokenUrl, basic::BasicClient,
    };
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio::sync::oneshot;

    if NOTION_CLIENT_ID == "FILL_ME_IN" {
        bail!(
            "MUR_NOTION_CLIENT_ID was not set at build time. Use --token <PAT> or rebuild mur with MUR_NOTION_CLIENT_ID=<your_client_id>."
        );
    }

    // Bind 127.0.0.1:0 to get an OS-assigned random port.
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .context("bind oauth callback")?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let client = BasicClient::new(
        ClientId::new(NOTION_CLIENT_ID.to_string()),
        None,
        AuthUrl::new(NOTION_OAUTH_AUTHORIZE.into())?,
        Some(TokenUrl::new(NOTION_OAUTH_TOKEN.into())?),
    )
    .set_redirect_uri(RedirectUrl::new(redirect_uri.clone())?);

    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
    let (auth_url, csrf) = client
        .authorize_url(CsrfToken::new_random)
        .set_pkce_challenge(pkce_challenge)
        .url();

    let (tx, rx) = oneshot::channel::<(String, String)>();
    let tx = Arc::new(tokio::sync::Mutex::new(Some(tx)));

    #[derive(serde::Deserialize)]
    struct CallbackParams {
        code: Option<String>,
        state: Option<String>,
    }

    let tx_clone = tx.clone();
    let app = Router::new().route(
        "/callback",
        get(move |Query(p): Query<CallbackParams>| {
            let tx = tx_clone.clone();
            async move {
                if let (Some(c), Some(s)) = (p.code, p.state)
                    && let Some(send) = tx.lock().await.take()
                {
                    let _ = send.send((c, s));
                }
                Html("<html><body><h2>Notion connected. You can close this tab.</h2></body></html>")
            }
        }),
    );

    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    println!("-> opening browser: {auth_url}");
    let _ = open::that(auth_url.as_str());

    let (code, returned_csrf) = tokio::time::timeout(Duration::from_secs(300), rx)
        .await
        .context("oauth callback timeout")?
        .context("callback channel closed")?;

    if returned_csrf != csrf.secret().as_str() {
        bail!("CSRF mismatch in OAuth callback");
    }

    let token_resp = client
        .exchange_code(AuthorizationCode::new(code))
        .set_pkce_verifier(pkce_verifier)
        .request_async(oauth2::reqwest::async_http_client)
        .await
        .context("notion token exchange")?;

    server.abort();

    let access = token_resp.access_token().secret().clone();

    // Notion's response includes workspace_id + workspace_name beyond the standard token fields.
    // The oauth2 crate ignores extras, so refetch via /v1/users/me to get workspace info.
    let client_http = Client::new();
    let me = client_http
        .get(format!("{NOTION_API_BASE}/users/me"))
        .bearer_auth(&access)
        .header("Notion-Version", NOTION_VERSION)
        .send()
        .await?;
    let me_json: serde_json::Value = me.json().await.unwrap_or_default();
    let workspace_name = me_json
        .get("bot")
        .and_then(|b| b.get("workspace_name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let workspace_id = me_json
        .get("bot")
        .and_then(|b| b.get("workspace_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(OAuthResult {
        access_token: access,
        workspace_id,
        workspace_name,
    })
}

#[cfg(not(feature = "server"))]
pub async fn run_oauth_flow() -> Result<OAuthResult> {
    bail!(
        "OAuth flow requires the 'server' feature (axum). Rebuild with default features or use --token <PAT>."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_extraction_from_real_property_shape() {
        let props = serde_json::json!({
            "Name": {
                "type": "title",
                "title": [{"plain_text": "My Page"}]
            }
        });
        assert_eq!(title_from_properties(Some(&props)), Some("My Page".into()));
    }

    #[test]
    fn missing_title_returns_none() {
        let props = serde_json::json!({"Status": {"type": "select", "select": null}});
        assert_eq!(title_from_properties(Some(&props)), None);
    }
}
