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
        bail!("catalog request failed: HTTP {}", resp.status());
    }
    Ok(resp
        .json::<CatalogResponse>()
        .await
        .context("parse catalog")?
        .items)
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
        401 => bail!("not authorized — log in again (`mur login`)"),
        402 | 403 => {
            bail!("'{id}' requires an active MUR Pro subscription — manage at app.mur.run")
        }
        s => bail!("download failed: HTTP {s}"),
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
}
