use anyhow::Result;
use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::*;

use crate::community;
use crate::gep;
use crate::store::yaml::YamlStore;
use crate::team;

pub(crate) async fn cmd_community_publish(name: &str) -> Result<()> {
    let store = YamlStore::default_store()?;
    let pattern = store.get(name)?;

    // Sanitize before publishing
    let mut publish_pattern = pattern.clone();
    community::sanitize_pattern(&mut publish_pattern);

    let description = publish_pattern.base.description.clone();

    let content = match &publish_pattern.base.content {
        Content::DualLayer {
            technical,
            principle,
        } => {
            if let Some(p) = principle {
                format!("{}\n\n---\n\n{}", technical, p)
            } else {
                technical.clone()
            }
        }
        Content::Plain(s) => s.clone(),
    };

    let mut tags: Vec<String> = publish_pattern.base.tags.languages.clone();
    tags.extend(publish_pattern.base.tags.topics.clone());

    let category = publish_pattern.base.tags.topics.first().cloned();

    let client = reqwest::Client::new();
    let resp = community::share(
        &client,
        name,
        &description,
        &content,
        &tags,
        category.as_deref(),
    )
    .await?;

    println!("  Published '{}' to community!", name);
    println!("  Pattern ID: {}", resp.pattern_id);
    Ok(())
}

pub(crate) async fn cmd_community_report(
    name: &str,
    effectiveness: f64,
    sessions: u32,
) -> Result<()> {
    if !(0.0..=1.0).contains(&effectiveness) {
        anyhow::bail!("Effectiveness must be between 0.0 and 1.0");
    }

    let client = reqwest::Client::new();
    let resp = community::report_effectiveness(&client, name, effectiveness, sessions).await?;
    println!("  {}", resp.message);
    Ok(())
}

pub(crate) async fn cmd_community_fetch(id: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let resp = community::copy_pattern(&client, id).await?;

    // Save as a session-tier pattern locally
    let mur_dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?
        .join(".mur");
    let patterns_dir = mur_dir.join("patterns");
    std::fs::create_dir_all(&patterns_dir)?;

    let slug = resp.name.to_lowercase().replace(' ', "-");
    let path = patterns_dir.join(format!("{}.yaml", slug));

    // Build a minimal Pattern and save as YAML
    let tags_vec: Vec<String> = match &resp.tags {
        serde_json::Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => vec![],
    };

    let pattern = mur_common::pattern::Pattern {
        base: KnowledgeBase {
            name: resp.name.clone(),
            description: resp.description.clone(),
            content: Content::Plain(resp.content.clone()),
            tier: Tier::Session,
            tags: Tags {
                languages: vec![],
                topics: tags_vec,
                extra: std::collections::HashMap::new(),
            },
            ..Default::default()
        },
        kind: None,
        origin: None,
        attachments: vec![],
    };

    let yaml = serde_yaml::to_string(&pattern)?;
    std::fs::write(&path, yaml)?;

    println!("  Fetched '{}' to {}", resp.name, path.display());
    Ok(())
}

pub(crate) async fn cmd_community_search(query: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let resp = community::search(&client, query).await?;

    if resp.patterns.is_empty() {
        println!("  No patterns found for '{}'", query);
        return Ok(());
    }

    println!("  Found {} pattern(s) for '{}':\n", resp.count, query);
    print_pattern_table(&resp.patterns);
    Ok(())
}

pub(crate) async fn cmd_community_list(sort: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let resp = community::list(&client, Some(sort)).await?;

    if resp.patterns.is_empty() {
        println!("  No community patterns yet.");
        return Ok(());
    }

    println!(
        "  {} community pattern(s) (sorted by {}):\n",
        resp.count, sort
    );
    print_pattern_table(&resp.patterns);
    Ok(())
}

pub(crate) async fn cmd_community_star(id: &str) -> Result<()> {
    let client = reqwest::Client::new();
    community::star(&client, id).await?;
    println!("  Starred pattern {}", id);
    Ok(())
}

pub(crate) async fn cmd_community_packs() -> Result<()> {
    let client = reqwest::Client::new();
    let resp = community::list_packs(&client).await?;

    if resp.packs.is_empty() {
        println!("  No community packs available yet.");
        return Ok(());
    }

    println!("  {} community pack(s):\n", resp.count);
    println!(
        "  {:<36}  {:<25}  {:>8}  {:<15}  TAGS",
        "ID", "NAME", "PATTERNS", "AUTHOR"
    );
    println!("  {}", "-".repeat(100));
    for pack in &resp.packs {
        let tags = pack.tags.join(", ");
        println!(
            "  {:<36}  {:<25}  {:>8}  {:<15}  {}",
            pack.id,
            truncate(&pack.name, 25),
            pack.pattern_count,
            truncate(&pack.author, 15),
            truncate(&tags, 30),
        );
    }
    println!("\n  Install a pack: mur community pack install <id>");
    Ok(())
}

pub(crate) async fn cmd_community_pack_show(id: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let resp = community::fetch_pack(&client, id).await?;

    println!("  Pack: {}", resp.pack.name);
    println!("  Description: {}", resp.pack.description);
    println!("  Author: {}", resp.pack.author);
    println!("  Patterns: {}", resp.pack.pattern_count);
    if !resp.pack.tags.is_empty() {
        println!("  Tags: {}", resp.pack.tags.join(", "));
    }
    println!();

    if !resp.patterns.is_empty() {
        println!("  Included patterns:");
        for p in &resp.patterns {
            println!("    - {} — {}", p.name, truncate(&p.description, 60));
        }
    }

    println!("\n  Install: mur community pack install {id}");
    Ok(())
}

pub(crate) async fn cmd_community_pack_install(id: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let resp = community::install_pack(&client, id).await?;

    let store = YamlStore::default_store()?;
    let mut installed = 0;

    for cp in &resp.patterns {
        let slug = cp.name.to_lowercase().replace(' ', "-");
        if store.exists(&slug) {
            println!("  Skipped '{}' (already exists)", cp.name);
            continue;
        }

        let tags_vec: Vec<String> = cp.tags.clone();
        let pattern = Pattern {
            base: KnowledgeBase {
                name: slug,
                description: cp.description.clone(),
                content: Content::Plain(cp.content.clone()),
                tier: Tier::Project,
                tags: Tags {
                    languages: vec![],
                    topics: tags_vec,
                    extra: std::collections::HashMap::new(),
                },
                ..Default::default()
            },
            kind: None,
            origin: Some(Origin {
                source: format!("pack:{}", id),
                trigger: OriginTrigger::Automatic,
                user: None,
                platform: None,
                confidence: 0.7,
            }),
            attachments: vec![],
        };
        store.save(&pattern)?;
        println!("  Installed: {}", cp.name);
        installed += 1;
    }

    println!(
        "\n  Installed {} of {} patterns from pack '{}'.",
        installed,
        resp.patterns.len(),
        resp.pack.name
    );
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
            origin: Some(Origin {
                source: format!("team:{}", team_id),
                trigger: OriginTrigger::CommunityShared,
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

pub(crate) fn cmd_gep_evolve() -> Result<()> {
    let store = YamlStore::default_store()?;
    let patterns = store.list_all()?;

    if patterns.is_empty() {
        println!("  No patterns to evolve.");
        return Ok(());
    }

    let before_stats = gep::population_stats(&patterns);
    println!(
        "  Before: {} patterns, avg fitness {:.3}",
        before_stats.count, before_stats.avg_fitness
    );

    // Use neutral feedback for evolution (the mutation step still recomputes fitness)
    let evolved = gep::evolve_generation(&patterns, &[]);

    // Save evolved patterns
    let mut updated = 0;
    for ep in &evolved {
        // Only save if the pattern name matches an existing one (skip crossover children)
        if store.exists(&ep.name) {
            store.save(ep)?;
            updated += 1;
        }
    }

    let after_stats = gep::population_stats(&evolved);
    println!(
        "  After:  {} patterns, avg fitness {:.3}",
        after_stats.count, after_stats.avg_fitness
    );
    println!("  Updated {} patterns.", updated);
    Ok(())
}

pub(crate) fn cmd_gep_status() -> Result<()> {
    let store = YamlStore::default_store()?;
    let patterns = store.list_all()?;

    if patterns.is_empty() {
        println!("  No patterns in store.");
        return Ok(());
    }

    let stats = gep::population_stats(&patterns);
    println!("  GEP Population Status");
    println!("  ─────────────────────");
    println!("  Patterns:           {}", stats.count);
    println!("  Avg fitness:        {:.3}", stats.avg_fitness);
    println!("  Max fitness:        {:.3}", stats.max_fitness);
    println!("  Min fitness:        {:.3}", stats.min_fitness);
    println!("  Avg effectiveness:  {:.3}", stats.avg_effectiveness);

    // Show top 5 by fitness
    let genes: Vec<gep::GepGene> = patterns
        .iter()
        .map(|p| gep::GepGene::from_pattern(p.clone()))
        .collect();
    let top = gep::select(&genes, 5);
    if !top.is_empty() {
        println!("\n  Top patterns by fitness:");
        for g in &top {
            println!(
                "    {:<30} fitness={:.3}  effectiveness={:.3}",
                g.pattern.name,
                g.fitness,
                g.pattern.evidence.effectiveness()
            );
        }
    }

    Ok(())
}

pub(crate) fn print_pattern_table(patterns: &[community::CommunityPattern]) {
    // Header
    println!(
        "  {:<36}  {:<30}  {:>5}  {:>5}  AUTHOR",
        "ID", "NAME", "STARS", "COPIES"
    );
    println!("  {}", "-".repeat(100));
    for p in patterns {
        let author = p.author_login.as_deref().unwrap_or(&p.author_name);
        println!(
            "  {:<36}  {:<30}  {:>5}  {:>5}  {}",
            p.id,
            truncate(&p.name, 30),
            p.star_count,
            p.copy_count,
            author
        );
    }
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max - 3])
    }
}
