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

/// `mur team list --team <id>`.
///
/// Lists the **legacy patterns** a team shared through the retired
/// `mur team share` command. They are still served by
/// `GET /api/v1/core/teams/{id}/patterns` so nobody loses sight of what was
/// shared, but they are not skills: skills are published to the registry via
/// `mur skill publish` and are not listed here.
pub(crate) async fn cmd_team_list(team_id: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let resp = team::list_team_patterns(&client, team_id).await?;
    print!("{}", render_team_patterns(team_id, &resp));
    Ok(())
}

/// Render the legacy-pattern listing. Pure so the wording is testable: this
/// output must never call the entries "skills" — the data source is the
/// legacy team-pattern endpoint, not the skill registry.
pub(crate) fn render_team_patterns(team_id: &str, resp: &team::TeamListResponse) -> String {
    let mut out = String::new();
    if resp.patterns.is_empty() {
        out.push_str(&format!(
            "  No legacy patterns shared in team '{}'.\n",
            team_id
        ));
        out.push_str(SKILLS_HINT);
        return out;
    }

    out.push_str(&format!(
        "  {} legacy pattern(s) in team '{}' (shared via the retired `mur team share`):\n\n",
        resp.count, team_id
    ));
    out.push_str(&format!(
        "  {:<30}  {:>8}  {:>12}  {:<15}  TAGS\n",
        "NAME", "MEMBERS", "EFFECTIVENESS", "SHARED BY"
    ));
    out.push_str(&format!("  {}\n", "-".repeat(90)));
    for p in &resp.patterns {
        let tags = p.tags.join(", ");
        out.push_str(&format!(
            "  {:<30}  {:>8}  {:>11.1}%  {:<15}  {}\n",
            truncate(&p.name, 30),
            p.members_using,
            p.combined_effectiveness * 100.0,
            truncate(&p.shared_by, 15),
            truncate(&tags, 25),
        ));
    }
    out.push_str(SKILLS_HINT);
    out
}

const SKILLS_HINT: &str = "  Skills are not listed here: publish with `mur skill publish`, find them with `mur skill search`.\n";

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max - 3])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team::{TeamListResponse, TeamPattern};

    fn pattern(name: &str) -> TeamPattern {
        TeamPattern {
            id: "p1".into(),
            name: name.into(),
            description: String::new(),
            content: String::new(),
            tags: vec!["rust".into()],
            team_id: "t1".into(),
            shared_by: "alice".into(),
            members_using: 3,
            combined_effectiveness: 0.5,
            total_sessions: 7,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    /// The listing reads the legacy team-pattern endpoint, so it must say so
    /// and must never present those entries as skills (which live in the
    /// registry and are not queried here).
    #[test]
    fn team_list_labels_entries_as_legacy_patterns_not_skills() {
        let resp = TeamListResponse {
            patterns: vec![pattern("retry-loop")],
            team_id: "t1".into(),
            count: 1,
        };
        let out = render_team_patterns("t1", &resp);
        assert!(out.contains("1 legacy pattern(s) in team 't1'"), "{out}");
        assert!(out.contains("retry-loop"), "{out}");
        assert!(out.contains("mur skill publish"), "{out}");
        assert!(
            !out.contains("skill(s)"),
            "must not label patterns as skills: {out}"
        );
    }

    #[test]
    fn team_list_empty_does_not_claim_no_skills() {
        let resp = TeamListResponse {
            patterns: vec![],
            team_id: "t1".into(),
            count: 0,
        };
        let out = render_team_patterns("t1", &resp);
        assert!(
            out.contains("No legacy patterns shared in team 't1'"),
            "{out}"
        );
        assert!(!out.contains("No skills"), "{out}");
        assert!(out.contains("mur skill publish"), "{out}");
    }
}
