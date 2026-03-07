#![allow(dead_code)]
//! Community API client for browsing, sharing, and fetching patterns.

use anyhow::{Context, Result};
use regex::Regex;
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

// ─── Effectiveness Reporting ────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct EffectivenessResponse {
    pub message: String,
}

/// Report how well a community pattern worked for you.
pub async fn report_effectiveness(
    client: &reqwest::Client,
    pattern_id: &str,
    effectiveness: f64,
    sessions_used: u32,
) -> Result<EffectivenessResponse> {
    let base = auth::server_url();
    let url = format!(
        "{}/api/v1/core/community/patterns/{}/effectiveness",
        base, pattern_id
    );

    let req = auth::auth_request(client, reqwest::Method::POST, &url).await?;

    let resp = req
        .json(&serde_json::json!({
            "effectiveness": effectiveness,
            "sessions_used": sessions_used,
        }))
        .send()
        .await
        .context("Failed to connect to mur server")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Report effectiveness failed ({}): {}", status, body);
    }

    resp.json().await.context("Invalid effectiveness response")
}

// ─── Sanitization ───────────────────────────────────────────────

/// Sanitize a pattern before publishing: strip personal paths, usernames, API keys.
pub fn sanitize_pattern(pattern: &mut mur_common::pattern::Pattern) {
    let scrub = |s: &mut String| {
        // Home directory paths (macOS, Linux, Windows)
        let home_re = Regex::new(r"(?i)/(?:Users|home)/[a-zA-Z0-9._-]+").unwrap();
        *s = home_re.replace_all(s, "/Users/<USER>").to_string();
        let win_re = Regex::new(r"(?i)C:\\Users\\[a-zA-Z0-9._-]+").unwrap();
        *s = win_re.replace_all(s, "C:\\Users\\<USER>").to_string();

        // API keys / tokens (common patterns: sk-..., ghp_..., Bearer ..., etc.)
        let key_re =
            Regex::new(r"(?i)(sk-|ghp_|gho_|github_pat_|xoxb-|xoxp-|Bearer\s+)[a-zA-Z0-9_-]{10,}")
                .unwrap();
        *s = key_re.replace_all(s, "<REDACTED>").to_string();

        // Generic long hex/base64 tokens (40+ chars of hex or alphanumeric)
        let token_re = Regex::new(r"\b[a-fA-F0-9]{40,}\b").unwrap();
        *s = token_re.replace_all(s, "<REDACTED_TOKEN>").to_string();
    };

    match &mut pattern.base.content {
        mur_common::pattern::Content::DualLayer {
            technical,
            principle,
        } => {
            scrub(technical);
            if let Some(p) = principle {
                scrub(p);
            }
        }
        mur_common::pattern::Content::Plain(s) => {
            scrub(s);
        }
    }

    // Also scrub description
    let mut desc = pattern.base.description.clone();
    let home_re = Regex::new(r"(?i)/(?:Users|home)/[a-zA-Z0-9._-]+").unwrap();
    desc = home_re.replace_all(&desc, "/Users/<USER>").to_string();
    let win_re = Regex::new(r"(?i)C:\\Users\\[a-zA-Z0-9._-]+").unwrap();
    desc = win_re.replace_all(&desc, "C:\\Users\\<USER>").to_string();
    pattern.base.description = desc;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_strips_home_paths() {
        let mut pattern = mur_common::pattern::Pattern {
            base: mur_common::knowledge::KnowledgeBase {
                name: "test".into(),
                description: "Config at /Users/david/.config".into(),
                content: mur_common::pattern::Content::Plain(
                    "Edit /Users/david/Projects/foo and /home/alice/bar".into(),
                ),
                ..Default::default()
            },
            kind: None,
            origin: None,
            attachments: vec![],
        };
        sanitize_pattern(&mut pattern);
        let text = pattern.base.content.as_text();
        assert!(!text.contains("david"));
        assert!(!text.contains("alice"));
        assert!(text.contains("<USER>"));
        assert!(!pattern.base.description.contains("david"));
    }

    #[test]
    fn test_sanitize_strips_api_keys() {
        let mut pattern = mur_common::pattern::Pattern {
            base: mur_common::knowledge::KnowledgeBase {
                name: "test".into(),
                description: "test".into(),
                content: mur_common::pattern::Content::Plain(
                    "Use key sk-1234567890abcdefghij for API access".into(),
                ),
                ..Default::default()
            },
            kind: None,
            origin: None,
            attachments: vec![],
        };
        sanitize_pattern(&mut pattern);
        let text = pattern.base.content.as_text();
        assert!(!text.contains("sk-1234567890"));
        assert!(text.contains("<REDACTED>"));
    }

    #[test]
    fn test_sanitize_dual_layer_content() {
        let mut pattern = mur_common::pattern::Pattern {
            base: mur_common::knowledge::KnowledgeBase {
                name: "test".into(),
                description: "test".into(),
                content: mur_common::pattern::Content::DualLayer {
                    technical: "Path: /home/bob/code".into(),
                    principle: Some("Token: ghp_abcdefghijklmnop1234".into()),
                },
                ..Default::default()
            },
            kind: None,
            origin: None,
            attachments: vec![],
        };
        sanitize_pattern(&mut pattern);
        if let mur_common::pattern::Content::DualLayer {
            technical,
            principle,
        } = &pattern.base.content
        {
            assert!(!technical.contains("bob"));
            assert!(!principle.as_ref().unwrap().contains("ghp_"));
        } else {
            panic!("Expected DualLayer");
        }
    }
}
