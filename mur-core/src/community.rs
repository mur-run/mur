#![allow(dead_code)]
//! Community API client for browsing, sharing, and fetching patterns.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::auth;

/// A community pattern as returned by the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommunityPattern {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub author_id: String,
    #[serde(default)]
    pub author_name: String,
    #[serde(default)]
    pub author_login: Option<String>,
    #[serde(default)]
    pub author_plan: String,
    #[serde(default)]
    pub copy_count: u64,
    #[serde(default)]
    pub view_count: u64,
    #[serde(default)]
    pub star_count: u64,
    #[serde(default)]
    pub starred: bool,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
pub struct PatternsListResponse {
    pub patterns: Vec<CommunityPattern>,
    #[serde(default)]
    pub count: u64,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SearchResponse {
    pub patterns: Vec<CommunityPattern>,
    #[serde(default)]
    pub count: u64,
    #[serde(default)]
    pub query: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ShareResponse {
    pub message: String,
    pub pattern_id: String,
}

#[derive(Debug, Deserialize)]
pub struct CopyResponse {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub tags: serde_json::Value,
    #[serde(default)]
    pub category: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct StatsResponse {
    #[serde(rename = "totalPatterns")]
    pub total_patterns: u64,
    #[serde(rename = "totalAuthors")]
    pub total_authors: u64,
    #[serde(rename = "totalCopies")]
    pub total_copies: u64,
    #[serde(rename = "totalStars")]
    pub total_stars: u64,
}

/// Search community patterns by query string.
pub async fn search(client: &reqwest::Client, query: &str) -> Result<SearchResponse> {
    let base = auth::server_url();
    let url = format!(
        "{}/api/v1/core/community/patterns/search?q={}",
        base,
        urlencoded(query)
    );

    let resp = client
        .get(&url)
        .send()
        .await
        .context("Failed to connect to mur server")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Search failed ({}): {}", status, body);
    }

    resp.json().await.context("Invalid search response")
}

/// List community patterns with optional sort.
pub async fn list(client: &reqwest::Client, sort: Option<&str>) -> Result<PatternsListResponse> {
    let base = auth::server_url();
    let mut url = format!("{}/api/v1/core/community/patterns", base);
    if let Some(sort) = sort {
        url = format!("{}?sort={}", url, urlencoded(sort));
    }

    let resp = client
        .get(&url)
        .send()
        .await
        .context("Failed to connect to mur server")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("List failed ({}): {}", status, body);
    }

    resp.json().await.context("Invalid list response")
}

/// Get popular patterns.
pub async fn popular(client: &reqwest::Client) -> Result<PatternsListResponse> {
    let base = auth::server_url();
    let url = format!("{}/api/v1/core/community/patterns/popular", base);

    let resp = client
        .get(&url)
        .send()
        .await
        .context("Failed to connect to mur server")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Failed to get popular patterns ({}): {}", status, body);
    }

    resp.json().await.context("Invalid popular response")
}

/// Get recent patterns.
pub async fn recent(client: &reqwest::Client) -> Result<PatternsListResponse> {
    let base = auth::server_url();
    let url = format!("{}/api/v1/core/community/patterns/recent", base);

    let resp = client
        .get(&url)
        .send()
        .await
        .context("Failed to connect to mur server")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Failed to get recent patterns ({}): {}", status, body);
    }

    resp.json().await.context("Invalid recent response")
}

/// Share (publish) a pattern to the community. Requires auth.
pub async fn share(
    client: &reqwest::Client,
    name: &str,
    description: &str,
    content: &str,
    tags: &[String],
    category: Option<&str>,
) -> Result<ShareResponse> {
    let base = auth::server_url();
    let url = format!("{}/api/v1/core/community/patterns/share", base);

    let req = auth::auth_request(client, reqwest::Method::POST, &url).await?;

    let mut body = serde_json::json!({
        "name": name,
        "description": description,
        "content": content,
        "tags": {
            "confirmed": tags,
            "inferred": []
        },
    });
    if let Some(cat) = category {
        body["category"] = serde_json::Value::String(cat.to_string());
    }

    let resp = req
        .json(&body)
        .send()
        .await
        .context("Failed to connect to mur server")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Share failed ({}): {}", status, body);
    }

    resp.json().await.context("Invalid share response")
}

/// Copy (fetch) a community pattern. Requires auth.
pub async fn copy_pattern(client: &reqwest::Client, pattern_id: &str) -> Result<CopyResponse> {
    let base = auth::server_url();
    let url = format!(
        "{}/api/v1/core/community/patterns/{}/copy",
        base, pattern_id
    );

    let req = auth::auth_request(client, reqwest::Method::POST, &url).await?;

    let resp = req
        .json(&serde_json::json!({}))
        .send()
        .await
        .context("Failed to connect to mur server")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Copy failed ({}): {}", status, body);
    }

    resp.json().await.context("Invalid copy response")
}

/// Star a community pattern. Requires auth.
pub async fn star(client: &reqwest::Client, pattern_id: &str) -> Result<()> {
    let base = auth::server_url();
    let url = format!(
        "{}/api/v1/core/community/patterns/{}/star",
        base, pattern_id
    );

    let req = auth::auth_request(client, reqwest::Method::POST, &url).await?;

    let resp = req
        .send()
        .await
        .context("Failed to connect to mur server")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Star failed ({}): {}", status, body);
    }

    Ok(())
}

/// Get community stats.
pub async fn stats(client: &reqwest::Client) -> Result<StatsResponse> {
    let base = auth::server_url();
    let url = format!("{}/api/v1/core/community/stats", base);

    let resp = client
        .get(&url)
        .send()
        .await
        .context("Failed to connect to mur server")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Stats failed ({}): {}", status, body);
    }

    resp.json().await.context("Invalid stats response")
}

// ─── Community Packs ────────────────────────────────────────────────

/// A curated collection of patterns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pack {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub pattern_count: u64,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
pub struct PacksListResponse {
    pub packs: Vec<Pack>,
    #[serde(default)]
    pub count: u64,
}

#[derive(Debug, Deserialize)]
pub struct PackDetailResponse {
    pub pack: Pack,
    pub patterns: Vec<CommunityPattern>,
}

/// List available community packs.
pub async fn list_packs(client: &reqwest::Client) -> Result<PacksListResponse> {
    let base = auth::server_url();
    let url = format!("{}/api/v1/core/community/packs", base);

    let resp = client
        .get(&url)
        .send()
        .await
        .context("Failed to connect to mur server")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("List packs failed ({status}): {body}");
    }

    resp.json().await.context("Invalid packs list response")
}

/// Fetch a pack by ID (returns pack info + its patterns).
pub async fn fetch_pack(client: &reqwest::Client, id: &str) -> Result<PackDetailResponse> {
    let base = auth::server_url();
    let url = format!("{}/api/v1/core/community/packs/{}", base, urlencoded(id));

    let resp = client
        .get(&url)
        .send()
        .await
        .context("Failed to connect to mur server")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Fetch pack failed ({status}): {body}");
    }

    resp.json().await.context("Invalid pack response")
}

/// Install a pack: fetch all its patterns and save them locally.
pub async fn install_pack(client: &reqwest::Client, id: &str) -> Result<PackDetailResponse> {
    let base = auth::server_url();
    let url = format!(
        "{}/api/v1/core/community/packs/{}/install",
        base,
        urlencoded(id)
    );

    let req = auth::auth_request(client, reqwest::Method::POST, &url).await?;

    let resp = req
        .json(&serde_json::json!({}))
        .send()
        .await
        .context("Failed to connect to mur server")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Install pack failed ({status}): {body}");
    }

    resp.json().await.context("Invalid install pack response")
}

fn urlencoded(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ' ' => "+".to_string(),
            c if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '~' => {
                c.to_string()
            }
            c => format!("%{:02X}", c as u32),
        })
        .collect()
}
