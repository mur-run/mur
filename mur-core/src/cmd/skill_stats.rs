//! `mur skill stats|pin|unpin|reindex-stats` CLI handlers (M5a).

use anyhow::Result;
use mur_common::skill::stats::SkillStats;

use super::agent::resolve_mur_home;

pub fn cmd_stats(name: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    let path = SkillStats::path(&home, name);
    match SkillStats::load(&path)? {
        Some(s) => println!("{}", serde_json::to_string_pretty(&s)?),
        None => println!(
            "no stats for skill '{}' — run `mur skill reindex-stats {}`",
            name, name
        ),
    }
    Ok(())
}

pub fn cmd_pin(name: &str, reason: Option<&str>) -> Result<()> {
    let home = resolve_mur_home()?;
    let path = SkillStats::path(&home, name);
    let reason_str = reason.unwrap_or("").to_string();
    SkillStats::merge_in_place(
        &path,
        || SkillStats::new(name, "unknown", "", chrono::Utc::now()),
        |s| {
            s.pinned = true;
            s.pinned_reason = reason_str.clone();
            Ok(())
        },
    )?;
    println!("pinned skill '{name}'");
    Ok(())
}

pub fn cmd_unpin(name: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    let path = SkillStats::path(&home, name);
    SkillStats::merge_in_place(
        &path,
        || SkillStats::new(name, "unknown", "", chrono::Utc::now()),
        |s| {
            s.pinned = false;
            s.pinned_reason = String::new();
            Ok(())
        },
    )?;
    println!("unpinned skill '{name}'");
    Ok(())
}

pub async fn cmd_reindex_stats(skill_filter: Option<&str>, days_back: u32) -> Result<()> {
    let home = resolve_mur_home()?;
    let report = crate::skill_stats::reindex::reindex_stats(
        &home,
        crate::skill_stats::reindex::ReindexOptions {
            skill_filter: skill_filter.map(str::to_string),
            since: None,
            days_back,
        },
    )?;
    println!(
        "Reindexed {} skill(s) from {} trace line(s)",
        report.skills_touched, report.lines_consumed
    );
    Ok(())
}
