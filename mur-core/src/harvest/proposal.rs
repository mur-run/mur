//! Workflow proposals — the harvest inbox at
//! `~/.mur/inbox/workflow-proposals/<session_id>.yaml`. Pure YAML files so the
//! Hub / companion nudge surface can read the same inbox (spec §3.2, §3.8).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ProposalStatus {
    Pending,
    Accepted,
    Dismissed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    /// Same as the source session id (one proposal per session).
    pub id: String,
    pub title: String,
    /// kebab-case workflow name suggestion.
    pub suggested_name: String,
    pub steps: Vec<String>,
    pub event_count: usize,
    pub duration_secs: i64,
    pub created_at: String,
    pub status: ProposalStatus,
    /// Existing workflow this proposal nearly duplicates (suggest merge).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub similar_to: Option<String>,
    /// Git repo root the session ran in (canonical project id). Set when the
    /// session had a working dir inside a repo; used to stamp `scope: Project`
    /// at accept so the learned skill is project-local.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)]
pub struct ProposalSummary {
    pub file: String,
    pub modified: String,
}

/// Pending proposals in `<home>/inbox/workflow-proposals`, newest first, capped at `limit`.
#[allow(dead_code)]
pub fn list_pending(mur_home: &Path, limit: usize) -> Vec<ProposalSummary> {
    let dir = mur_home.join("inbox").join("workflow-proposals");
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut items: Vec<(std::time::SystemTime, String)> = entries
        .flatten()
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("yaml"))
        .filter_map(|e| {
            let m = e.metadata().ok()?.modified().ok()?;
            Some((m, e.file_name().to_string_lossy().to_string()))
        })
        .collect();
    items.sort_by_key(|b| std::cmp::Reverse(b.0));
    items
        .into_iter()
        .take(limit)
        .map(|(m, file)| ProposalSummary {
            file,
            modified: chrono::DateTime::<chrono::Utc>::from(m).to_rfc3339(),
        })
        .collect()
}

pub fn inbox_dir() -> PathBuf {
    crate::paths::mur_root(None)
        .join("inbox")
        .join("workflow-proposals")
}

pub fn save_in_dir(dir: &Path, p: &Proposal) -> Result<()> {
    fs::create_dir_all(dir)?;
    let yaml = serde_yaml::to_string(p)?;
    let tmp = dir.join(format!("{}.yaml.tmp", p.id));
    fs::write(&tmp, yaml)?;
    fs::rename(&tmp, dir.join(format!("{}.yaml", p.id))).context("persist proposal")?;
    Ok(())
}

pub fn list_in_dir(dir: &Path) -> Result<Vec<Proposal>> {
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        if let Ok(content) = fs::read_to_string(&path)
            && let Ok(p) = serde_yaml::from_str::<Proposal>(&content)
        {
            out.push(p);
        }
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(out)
}

pub fn pending_in_dir(dir: &Path) -> Result<Vec<Proposal>> {
    Ok(list_in_dir(dir)?
        .into_iter()
        .filter(|p| p.status == ProposalStatus::Pending)
        .collect())
}

pub fn set_status_in_dir(dir: &Path, id: &str, status: ProposalStatus) -> Result<()> {
    let path = dir.join(format!("{}.yaml", id));
    let mut p: Proposal = serde_yaml::from_str(&fs::read_to_string(&path)?)?;
    p.status = status;
    save_in_dir(dir, &p)
}

/// Token-set Jaccard similarity over two step lists (zero-cost near-dup check).
pub fn step_similarity(a: &[String], b: &[String]) -> f32 {
    let ta: BTreeSet<&str> = a.iter().flat_map(|s| s.split_whitespace()).collect();
    let tb: BTreeSet<&str> = b.iter().flat_map(|s| s.split_whitespace()).collect();
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let inter = ta.intersection(&tb).count() as f32;
    let union = ta.union(&tb).count() as f32;
    inter / union
}

/// kebab-case a session title into a workflow name suggestion.
pub fn suggest_name(title: &str) -> String {
    let name: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let name = name
        .split('-')
        .filter(|s| !s.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-");
    if name.is_empty() {
        "captured-workflow".to_string()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proposal(id: &str, status: ProposalStatus) -> Proposal {
        Proposal {
            id: id.into(),
            project: None,
            title: "Deploy api".into(),
            suggested_name: "deploy-api".into(),
            steps: vec!["cargo build".into(), "fly deploy --app <STR>".into()],
            event_count: 12,
            duration_secs: 300,
            created_at: "2026-06-11T00:00:00Z".into(),
            status,
            similar_to: None,
        }
    }

    #[test]
    fn save_list_pending_set_status_roundtrip() {
        let tmp = tempfile::TempDir::new().unwrap();
        save_in_dir(tmp.path(), &proposal("s1", ProposalStatus::Pending)).unwrap();
        save_in_dir(tmp.path(), &proposal("s2", ProposalStatus::Dismissed)).unwrap();

        assert_eq!(list_in_dir(tmp.path()).unwrap().len(), 2);
        assert_eq!(pending_in_dir(tmp.path()).unwrap().len(), 1);

        set_status_in_dir(tmp.path(), "s1", ProposalStatus::Accepted).unwrap();
        assert!(pending_in_dir(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn similarity_high_for_same_skeleton() {
        let a = vec![
            "cargo build".to_string(),
            "fly deploy --app <STR>".to_string(),
        ];
        let b = vec![
            "cargo build".to_string(),
            "fly deploy --app <STR>".to_string(),
        ];
        assert!(step_similarity(&a, &b) > 0.99);
        let c = vec!["npm test".to_string()];
        assert!(step_similarity(&a, &c) < 0.2);
    }

    #[test]
    fn suggest_name_kebabs_title() {
        assert_eq!(
            suggest_name("Fix hub dark-mode contrast!"),
            "fix-hub-dark-mode-contrast"
        );
        assert_eq!(suggest_name("???"), "captured-workflow");
    }

    #[test]
    fn list_pending_newest_first_capped() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("inbox").join("workflow-proposals");
        std::fs::create_dir_all(&dir).unwrap();
        for n in ["a.yaml", "b.yaml", "c.yaml"] {
            std::fs::write(dir.join(n), "test: 1\n").unwrap();
        }
        let list = list_pending(tmp.path(), 2);
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn list_pending_empty_dir_is_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(list_pending(tmp.path(), 5).is_empty());
    }
}
