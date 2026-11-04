//! `mur team list|use` — team membership and default-team selection.
//!
//! `mur team share` / `mur team sync` were retired with the local Pattern
//! store (Tier C3); skills are shared through the registry
//! (`mur skill publish` / `mur skill install`).

use anyhow::Result;

use crate::store::config as store_config;
use crate::team;

pub(crate) async fn cmd_team_use(slug: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let team_id = team::resolve_team_id(&client, slug).await?;

    let mut config = store_config::load_config()?;
    config.sync.team_id = Some(team_id.clone());
    store_config::save_config(&config)?;

    // Show which team was selected
    let teams = team::list_user_teams(&client).await?;
    if let Some(t) = teams.iter().find(|t| t.id == team_id) {
        println!("  Default team set to: {} ({})", t.name, t.slug);
    } else {
        println!("  Default team set to: {}", team_id);
    }
    Ok(())
}

pub(crate) async fn cmd_team_list_mine() -> Result<()> {
    let client = reqwest::Client::new();
    let teams = team::list_user_teams(&client).await?;

    if teams.is_empty() {
        println!("  You are not a member of any teams.");
        println!("  Visit https://app.mur.run to create or join a team.");
        return Ok(());
    }

    println!("  {:<36}  {:<25}  {:<15}  ROLE", "ID", "NAME", "SLUG");
    println!("  {}", "-".repeat(88));
    for t in &teams {
        println!(
            "  {:<36}  {:<25}  {:<15}  {}",
            t.id,
            truncate(&t.name, 25),
            truncate(&t.slug, 15),
            t.role,
        );
    }
    println!();
    Ok(())
}

pub(crate) async fn cmd_team_list(team_id: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let resp = team::list_team_patterns(&client, team_id).await?;

    if resp.patterns.is_empty() {
        println!("  No skills shared in team '{}'.", team_id);
        return Ok(());
    }

    println!("  {} skill(s) in team '{}':\n", resp.count, team_id);
    println!(
        "  {:<30}  {:>8}  {:>12}  {:<15}  TAGS",
        "NAME", "MEMBERS", "EFFECTIVENESS", "SHARED BY"
    );
    println!("  {}", "-".repeat(90));
    for p in &resp.patterns {
        let tags = p.tags.join(", ");
        println!(
            "  {:<30}  {:>8}  {:>11.1}%  {:<15}  {}",
            truncate(&p.name, 30),
            p.members_using,
            p.combined_effectiveness * 100.0,
            truncate(&p.shared_by, 15),
            truncate(&tags, 25),
        );
    }
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max - 3])
    }
}
