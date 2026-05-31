use anyhow::Result;
use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::*;

use crate::community;
use crate::store::config as store_config;
use crate::store::yaml::YamlStore;
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
    println!("  Run `mur team sync` to pull patterns.");
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
        println!("  No patterns shared in team '{}'.", team_id);
        return Ok(());
    }

    println!("  {} pattern(s) in team '{}':\n", resp.count, team_id);
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

pub(crate) async fn cmd_team_share(name: &str, team_id: &str) -> Result<()> {
    let store = YamlStore::default_store()?;
    let pattern = store.get(name)?;

    let mut tags: Vec<String> = pattern.base.tags.languages.clone();
    tags.extend(pattern.base.tags.topics.clone());

    // Sanitize before sharing
    let mut sanitize_pattern = pattern.clone();
    community::sanitize_pattern(&mut sanitize_pattern);

    let sanitized_content = sanitize_pattern.base.content.as_text();
    let sanitized_desc = sanitize_pattern.base.description.clone();

    let client = reqwest::Client::new();
    let resp = team::share_to_team(
        &client,
        team_id,
        name,
        &sanitized_desc,
        &sanitized_content,
        &tags,
    )
    .await?;

    println!("  Shared '{}' to team '{}'!", name, team_id);
    println!("  Pattern ID: {}", resp.pattern_id);
    Ok(())
}

pub(crate) async fn cmd_team_sync(team_id: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let resp = team::sync_team(&client, team_id).await?;

    if resp.patterns.is_empty() {
        println!("  No new patterns to sync from team '{}'.", team_id);
        return Ok(());
    }

    let store = YamlStore::default_store()?;
    let mut synced = 0;

    for tp in &resp.patterns {
        let slug = tp.name.to_lowercase().replace(' ', "-");
        if store.exists(&slug) {
            println!("  Skipped '{}' (already exists)", tp.name);
            continue;
        }

        let pattern = Pattern {
            base: KnowledgeBase {
                name: slug,
                description: tp.description.clone(),
                content: Content::Plain(tp.content.clone()),
                tier: Tier::Project,
                tags: Tags {
                    languages: vec![],
                    topics: tp.tags.clone(),
                    extra: std::collections::HashMap::new(),
                },
                ..Default::default()
            },
            kind: None,
            #[allow(deprecated)] // transitional: user/platform fields being phased out
            origin: Some(Origin {
                source: format!("team:{}", team_id),
                trigger: OriginTrigger::CommunityShared,
                actor: None,
                user: None,
                platform: None,
                confidence: 0.7,
            }),
            attachments: vec![],
        };
        store.save(&pattern)?;
        println!("  Synced: {}", tp.name);
        synced += 1;
    }

    println!(
        "\n  Synced {} of {} patterns from team '{}'.",
        synced,
        resp.patterns.len(),
        team_id
    );
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max - 3])
    }
}
