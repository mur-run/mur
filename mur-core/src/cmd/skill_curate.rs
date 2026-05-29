//! `mur skill curate <name>` — record a human curation event so an
//! LLM-extracted skill can promote past Emerging (amendment A1).

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use mur_common::telemetry::METHOD_SKILL_CURATED;
use std::path::Path;

use super::agent::resolve_mur_home;

/// Append a `mur.skill.curated` event to today's trace log. The stats
/// reducer (`reindex_stats`) turns this into `SkillStats::curated_at`.
pub fn record_curation(mur_home: &Path, skill_name: &str, now: DateTime<Utc>) -> Result<()> {
    let traces_dir = mur_home.join("traces");
    std::fs::create_dir_all(&traces_dir)
        .with_context(|| format!("create {}", traces_dir.display()))?;
    let path = traces_dir
        .join(now.format("%Y-%m-%d").to_string())
        .with_extension("jsonl");

    let line = serde_json::json!({
        "ts": now.to_rfc3339(),
        "method": METHOD_SKILL_CURATED,
        "mur.skill.name": skill_name,
    });

    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    writeln!(f, "{}", serde_json::to_string(&line)?)?;
    Ok(())
}

/// CLI handler for `mur skill curate <name>`.
pub fn cmd_curate(name: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    record_curation(&home, name, Utc::now())?;
    println!(
        "Curated '{name}'. Run `mur skill reindex-stats {name}` then `mur skill sweep` \
         to let it promote past Emerging."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_curation_appends_a_curated_event() {
        let tmp = tempfile::tempdir().unwrap();
        let now = Utc::now();
        record_curation(tmp.path(), "deploy", now).unwrap();

        let path = tmp
            .path()
            .join("traces")
            .join(now.format("%Y-%m-%d").to_string())
            .with_extension("jsonl");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("mur.skill.curated"));
        assert!(content.contains("\"mur.skill.name\":\"deploy\""));
    }

    #[test]
    fn record_curation_appends_not_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let now = Utc::now();
        record_curation(tmp.path(), "a", now).unwrap();
        record_curation(tmp.path(), "b", now).unwrap();
        let path = tmp
            .path()
            .join("traces")
            .join(now.format("%Y-%m-%d").to_string())
            .with_extension("jsonl");
        let lines = std::fs::read_to_string(&path).unwrap();
        assert_eq!(lines.lines().count(), 2);
    }
}
