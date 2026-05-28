#![allow(dead_code)]
//! Team shared patterns — share and sync patterns within a team.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::auth;

/// A team pattern with aggregated evidence from all team members.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamPattern {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub team_id: String,
    #[serde(default)]
    pub shared_by: String,
    #[serde(default)]
    pub members_using: u32,
    #[serde(default)]
    pub combined_effectiveness: f64,
    #[serde(default)]
    pub total_sessions: u64,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
pub struct TeamListResponse {
    pub patterns: Vec<TeamPattern>,
    #[serde(default)]
    pub team_id: String,
    #[serde(default)]
    pub count: u64,
}

#[derive(Debug, Deserialize)]
pub struct TeamShareResponse {
    pub message: String,
    pub pattern_id: String,
}

#[derive(Debug, Deserialize)]
pub struct TeamSyncResponse {
    pub patterns: Vec<TeamPattern>,
    #[serde(default)]
    pub count: u64,
}

/// A team the current user belongs to.
#[derive(Debug, Clone, Deserialize)]
pub struct UserTeam {
    pub id: String,
    pub name: String,
    pub slug: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub pattern_count: u64,
}

#[derive(Debug, Deserialize)]
struct ListUserTeamsResponse {
    teams: Vec<UserTeam>,
}

/// List teams the current user belongs to.
pub async fn list_user_teams(client: &reqwest::Client) -> Result<Vec<UserTeam>> {
    let base = auth::server_url();
    let url = format!("{}/api/v1/core/teams/", base);

    let req = auth::auth_request(client, reqwest::Method::GET, &url).await?;
    let resp = req
        .send()
        .await
        .context("Failed to connect to mur server")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("List teams failed ({}): {}", status, body);
    }

    let data: ListUserTeamsResponse = resp.json().await.context("Invalid list teams response")?;
    Ok(data.teams)
}

fn is_uuid(s: &str) -> bool {
    // xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx (8-4-4-4-12, 36 chars)
    let b = s.as_bytes();
    b.len() == 36
        && b[8] == b'-'
        && b[13] == b'-'
        && b[18] == b'-'
        && b[23] == b'-'
        && b.iter().enumerate().all(|(i, &c)| {
            if i == 8 || i == 13 || i == 18 || i == 23 {
                true
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

/// Resolve a team identifier (slug or UUID) to a UUID.
pub async fn resolve_team_id(client: &reqwest::Client, team_id_or_slug: &str) -> Result<String> {
    if is_uuid(team_id_or_slug) {
        return Ok(team_id_or_slug.to_string());
    }

    // Otherwise, look up by slug or name
    let teams = list_user_teams(client).await?;
    for team in &teams {
        if team.slug.eq_ignore_ascii_case(team_id_or_slug)
            || team.name.eq_ignore_ascii_case(team_id_or_slug)
        {
            return Ok(team.id.clone());
        }
    }

    anyhow::bail!(
        "Team '{}' not found. Use 'mur team ls' to see your teams.",
        team_id_or_slug
    )
}

/// List patterns shared in a team.
pub async fn list_team_patterns(
    client: &reqwest::Client,
    team_id: &str,
) -> Result<TeamListResponse> {
    let base = auth::server_url();
    let url = format!("{}/api/v1/core/teams/{}/patterns", base, team_id);

    let req = auth::auth_request(client, reqwest::Method::GET, &url).await?;

    let resp = req
        .send()
        .await
        .context("Failed to connect to mur server")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("List team patterns failed ({}): {}", status, body);
    }

    resp.json().await.context("Invalid team patterns response")
}

/// Share a pattern to a team.
pub async fn share_to_team(
    client: &reqwest::Client,
    team_id: &str,
    name: &str,
    description: &str,
    content: &str,
    tags: &[String],
) -> Result<TeamShareResponse> {
    let base = auth::server_url();
    let url = format!("{}/api/v1/core/teams/{}/patterns", base, team_id);

    let req = auth::auth_request(client, reqwest::Method::POST, &url).await?;

    let resp = req
        .json(&serde_json::json!({
            "name": name,
            "description": description,
            "content": content,
            "tags": tags,
        }))
        .send()
        .await
        .context("Failed to connect to mur server")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Share to team failed ({}): {}", status, body);
    }

    resp.json().await.context("Invalid team share response")
}

/// Sync (pull) latest team patterns.
pub async fn sync_team(client: &reqwest::Client, team_id: &str) -> Result<TeamSyncResponse> {
    let base = auth::server_url();
    let url = format!("{}/api/v1/core/teams/{}/sync/pull?since=0", base, team_id);

    let req = auth::auth_request(client, reqwest::Method::GET, &url).await?;

    let resp = req
        .send()
        .await
        .context("Failed to connect to mur server")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Team sync failed ({}): {}", status, body);
    }

    resp.json().await.context("Invalid team sync response")
}
