//! Team membership lookups plus the read-only listing of legacy team patterns.
//!
//! Sharing/syncing patterns (`mur team share|sync`) was retired with the local
//! Pattern store (Tier C3); skills go through the registry instead.

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
    /// Echoed by the server; kept so the wire shape stays documented.
    #[serde(default)]
    #[allow(dead_code)]
    pub team_id: String,
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
    /// Legacy pattern count reported by the server; not displayed since Tier C3.
    #[serde(default)]
    #[allow(dead_code)]
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

/// List the legacy patterns shared in a team (via the retired `mur team share`).
/// Skills are not served here; they live in the registry.
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
