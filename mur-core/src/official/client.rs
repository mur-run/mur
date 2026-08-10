//! HTTP client for the app.mur.run official catalog API.
use anyhow::{Context, Result, bail};
use mur_common::official::OfficialLicense;
use serde::Deserialize;

/// Catalog item as returned by `GET /api/v1/core/catalog`. Only the fields the
/// client acts on are captured; any additional server fields (e.g. `kind`,
/// `name`) are ignored by serde.
#[derive(Debug, Clone, Deserialize)]
pub struct CatalogItem {
    pub id: String,
    #[serde(default)]
    pub tier: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Deserialize)]
struct CatalogResponse {
    items: Vec<CatalogItem>,
}

#[derive(Deserialize)]
struct DownloadResponse {
    license: OfficialLicense,
    bundle_base64: String,
}

/// Public listing — no auth.
pub async fn fetch_catalog(client: &reqwest::Client, base: &str) -> Result<Vec<CatalogItem>> {
    let url = format!("{base}/api/v1/core/catalog");
    let resp = client.get(&url).send().await.context("fetch catalog")?;
    if !resp.status().is_success() {
        bail!("catalog request failed: {}", describe_status(resp).await);
    }
    Ok(resp
        .json::<CatalogResponse>()
        .await
        .context("parse catalog")?
        .items)
}

/// How much of a server error body to keep. Enough for a JSON `{"error": …}`,
/// short enough that an HTML error page cannot bury the status it came with.
const ERROR_BODY_LIMIT: usize = 300;

/// Render a failed response as `HTTP <status>: <body>`.
///
/// The status alone is what the server could not say: a 503 from the catalog
/// means "route is up, index unavailable", and only the body names which. Body
/// handling degrades rather than raises — an unreadable or blank body falls
/// back to the bare status, newlines collapse so one failure stays one line,
/// and truncation counts characters so a multi-byte body cannot panic.
async fn describe_status(resp: reqwest::Response) -> String {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let body = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if body.is_empty() {
        return format!("HTTP {status}");
    }
    if body.chars().count() > ERROR_BODY_LIMIT {
        let head: String = body.chars().take(ERROR_BODY_LIMIT).collect();
        return format!("HTTP {status}: {head}…");
    }
    format!("HTTP {status}: {body}")
}

/// Authenticated download: returns (bundle bytes, license).
pub async fn download_item(
    client: &reqwest::Client,
    base: &str,
    access_token: &str,
    id: &str,
) -> Result<(Vec<u8>, OfficialLicense)> {
    use base64::{Engine, engine::general_purpose::STANDARD as B64};
    let url = format!("{base}/api/v1/core/catalog/{id}/download");
    let resp = client
        .get(&url)
        .bearer_auth(access_token)
        .send()
        .await
        .context("download item")?;
    match resp.status().as_u16() {
        200 => {}
        401 => bail!("not authorized — log in again (`mur auth login`)"),
        402 | 403 => {
            bail!("'{id}' requires an active MUR Pro subscription — manage at app.mur.run")
        }
        // Anything else is unmapped, so the body is the only explanation there is.
        _ => bail!("download failed: {}", describe_status(resp).await),
    }
    let body: DownloadResponse = resp.json().await.context("parse download response")?;
    let bytes = B64.decode(&body.bundle_base64).context("decode bundle")?;
    Ok((bytes, body.license))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn fetch_catalog_parses_items() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/core/catalog"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "items": [{"id":"fleets/deep-research","kind":"fleet","name":"deep-research",
                           "tier":"pro","version":"1.0.0","description":"d"}]
            })))
            .mount(&server)
            .await;
        let items = fetch_catalog(&reqwest::Client::new(), &server.uri())
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "fleets/deep-research");
    }

    #[tokio::test]
    async fn download_decodes_bundle_and_license() {
        use base64::{Engine, engine::general_purpose::STANDARD as B64};
        let server = MockServer::start().await;
        let lic = serde_json::json!({
            "format_version":1,"user_id":"u1","item":"fleets/x","version":"1.0.0",
            "expires_at":"2027-01-01T00:00:00Z","signer_pubkey":"", "sig":"s"
        });
        Mock::given(method("GET"))
            .and(path("/api/v1/core/catalog/fleets/x/download"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "license": lic, "bundle_base64": B64.encode(b"BUNDLE")
            })))
            .mount(&server)
            .await;
        let (bytes, license) =
            download_item(&reqwest::Client::new(), &server.uri(), "tok", "fleets/x")
                .await
                .unwrap();
        assert_eq!(bytes, b"BUNDLE");
        assert_eq!(license.item, "fleets/x");
    }

    #[tokio::test]
    async fn download_maps_entitlement_errors() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/core/catalog/fleets/x/download"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let err = download_item(&reqwest::Client::new(), &server.uri(), "tok", "fleets/x")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("subscription"), "{err}");
    }

    /// The status alone is what the server could NOT say. A live catalog route
    /// with no published index answers 503 and explains itself in the body;
    /// without it the message is indistinguishable from any other 503.
    #[tokio::test]
    async fn catalog_error_carries_the_server_explanation() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/core/catalog"))
            .respond_with(ResponseTemplate::new(503).set_body_json(serde_json::json!({
                "error": "official catalog unavailable: index.json request failed: status 404"
            })))
            .mount(&server)
            .await;
        let err = fetch_catalog(&reqwest::Client::new(), &server.uri())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("503"), "{err}");
        assert!(err.contains("index.json request failed"), "{err}");
    }

    /// An unmapped download failure has no curated message, so the body is the
    /// only explanation there is.
    #[tokio::test]
    async fn download_error_carries_the_server_explanation() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/core/catalog/fleets/x/download"))
            .respond_with(ResponseTemplate::new(500).set_body_string("storage backend down"))
            .mount(&server)
            .await;
        let err = download_item(&reqwest::Client::new(), &server.uri(), "tok", "fleets/x")
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("500") && err.contains("storage backend down"),
            "{err}"
        );
    }

    /// Bodies degrade rather than raise: blank ones fall back to the bare
    /// status, long ones truncate, and newlines collapse so one failure stays
    /// one log line.
    #[tokio::test]
    async fn error_bodies_degrade_instead_of_raising() {
        async fn describe(body: &str) -> String {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/api/v1/core/catalog"))
                .respond_with(ResponseTemplate::new(500).set_body_string(body))
                .mount(&server)
                .await;
            fetch_catalog(&reqwest::Client::new(), &server.uri())
                .await
                .unwrap_err()
                .to_string()
        }

        assert_eq!(
            describe("   \n  ").await,
            "catalog request failed: HTTP 500 Internal Server Error"
        );
        assert!(describe(&"x".repeat(5000)).await.ends_with('…'));
        let multi = describe("line one\nline two").await;
        assert!(
            !multi.contains('\n') && multi.contains("line one line two"),
            "{multi}"
        );
        // Multi-byte bodies truncate on character boundaries, never panic.
        assert!(describe(&"漢".repeat(5000)).await.ends_with('…'));
    }
}
