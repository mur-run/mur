//! Parse and load skill references.
//!
//! A ref is either `<name>` (local on invoking agent) or
//! `agent://<peer>/<name>` (read-only from peer).

use anyhow::{Result, anyhow, bail};
use chrono::Utc;
use mur_common::skill::manifest::SkillManifest;
use mur_common::skill::parser::parse_canonical;
use mur_common::skill::stats::SkillStats;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRef {
    /// `None` = local current agent; `Some(name)` = peer agent.
    pub agent: Option<String>,
    pub skill: String,
}

impl SkillRef {
    pub fn display(&self) -> String {
        match &self.agent {
            Some(a) => format!("agent://{a}/{}", self.skill),
            None => format!("local/{}", self.skill),
        }
    }
}

pub fn parse_ref(s: &str) -> Result<SkillRef> {
    if let Some(rest) = s.strip_prefix("agent://") {
        let mut parts = rest.splitn(2, '/');
        let agent = parts
            .next()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("missing peer agent in ref '{s}'"))?;
        let skill = parts
            .next()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("missing skill name in ref '{s}'"))?;
        Ok(SkillRef {
            agent: Some(agent.to_string()),
            skill: skill.to_string(),
        })
    } else if s.contains('/') {
        bail!("invalid skill ref '{s}': use '<name>' or 'agent://<peer>/<name>'");
    } else {
        Ok(SkillRef {
            agent: None,
            skill: s.to_string(),
        })
    }
}

#[derive(Debug)]
pub struct LoadedSkillRef {
    pub manifest: SkillManifest,
    pub stats: SkillStats,
    /// `"local"` or the peer agent name — for display + EvolutionEvent.
    pub agent_label: String,
    pub ref_: SkillRef,
}

/// Load manifest + stats for a `SkillRef`. `current_agent` is the invoking
/// agent name and is used to resolve `agent: None` (local) refs to the
/// invoker's per-agent skills directory.
pub fn load_skill_ref(
    home: &Path,
    current_agent: &str,
    r: &SkillRef,
) -> Result<LoadedSkillRef> {
    let agent_name = r.agent.as_deref().unwrap_or(current_agent);
    let agent_label = r.agent.clone().unwrap_or_else(|| "local".to_string());

    let agent_root = home.join("agents").join(agent_name);
    if !agent_root.exists() {
        bail!("agent '{agent_name}' not found at {}", agent_root.display());
    }

    let manifest_path = agent_root.join("skills").join(&r.skill).join("skill.yaml");
    if !manifest_path.exists() {
        let installed = installed_skills(&agent_root);
        bail!(
            "skill '{}' not found on agent '{agent_name}'. Installed: {}",
            r.skill,
            installed.join(", ")
        );
    }

    let yaml = std::fs::read_to_string(&manifest_path)?;
    let manifest =
        parse_canonical(&yaml).map_err(|e| anyhow!("parse {manifest_path:?}: {e}"))?;

    let stats_path = SkillStats::path_agent(home, agent_name, &r.skill);
    let stats = SkillStats::load(&stats_path)?
        .unwrap_or_else(|| SkillStats::new(&r.skill, "unknown", "", Utc::now()));

    Ok(LoadedSkillRef {
        manifest,
        stats,
        agent_label,
        ref_: r.clone(),
    })
}

/// Write initial stats for a newly created skill.
pub fn write_initial_stats(home: &Path, agent: &str, name: &str, stats: &SkillStats) -> Result<()> {
    let path = SkillStats::path_agent(home, agent, name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(stats)?;
    std::fs::write(&path, json)?;
    Ok(())
}

fn installed_skills(agent_root: &Path) -> Vec<String> {
    let dir = agent_root.join("skills");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return vec![];
    };
    let mut out: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_local_ref() {
        let r = parse_ref("research-prices").unwrap();
        assert_eq!(r.agent, None);
        assert_eq!(r.skill, "research-prices");
        assert_eq!(r.display(), "local/research-prices");
    }

    #[test]
    fn parse_peer_ref() {
        let r = parse_ref("agent://bob/lookup").unwrap();
        assert_eq!(r.agent.as_deref(), Some("bob"));
        assert_eq!(r.skill, "lookup");
        assert_eq!(r.display(), "agent://bob/lookup");
    }

    #[test]
    fn parse_rejects_bare_slash() {
        assert!(parse_ref("foo/bar").is_err());
    }

    #[test]
    fn parse_rejects_empty_agent_or_skill() {
        assert!(parse_ref("agent:///bar").is_err());
        assert!(parse_ref("agent://bob/").is_err());
    }

    #[test]
    fn load_errors_when_agent_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let r = SkillRef {
            agent: Some("ghost".into()),
            skill: "x".into(),
        };
        let err = load_skill_ref(tmp.path(), "self", &r).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }
}
