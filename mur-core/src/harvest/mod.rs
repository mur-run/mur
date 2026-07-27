//! Harvest pipeline (spec 2026-06-11 §3.2): heuristic gate over ambient
//! recordings → skeletonized workflow proposals → `mur out` review.
//!
//! `scan_in_dirs` is the testable core; `scan()` binds it to `~/.mur`. The
//! proposals it writes are also the candidate feed for the companion nudge
//! surface (2026-05-29 nudge spec) — that emission path is owned by the nudge
//! plan, not this module.

pub mod gate;
pub mod proposal;
pub mod skeleton;

use anyhow::Result;
use mur_common::config::HarvestCfg;
use std::path::Path;

pub struct ScanReport {
    pub scanned: usize,
    pub proposed: usize,
}

/// Gate every un-gated, idle session; write a proposal per passing session.
/// Sets `gated_at` on every scanned session so a session is judged exactly once.
pub fn scan_in_dirs(
    recordings_dir: &Path,
    inbox_dir: &Path,
    existing_workflow_steps: &[(String, Vec<String>)],
    cfg: &HarvestCfg,
) -> Result<ScanReport> {
    let mut report = ScanReport {
        scanned: 0,
        proposed: 0,
    };
    if !recordings_dir.exists() {
        return Ok(report);
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let idle_ms = (cfg.idle_minutes.max(0) as u64) * 60 * 1000;

    for entry in std::fs::read_dir(recordings_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else {
            continue;
        };
        let meta_path = recordings_dir.join(format!("{}.meta.json", id));
        let Some(mut meta) = std::fs::read_to_string(&meta_path)
            .ok()
            .and_then(|c| serde_json::from_str::<crate::session::SessionMeta>(&c).ok())
        else {
            continue;
        };
        if meta.gated_at.is_some() {
            continue;
        }

        let events: Vec<crate::session::SessionEvent> = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();

        // Idle check: session must be over (no event for idle_minutes), unless marked.
        let last_ts = events.last().map(|e| e.timestamp).unwrap_or(0);
        if !meta.marked && now_ms.saturating_sub(last_ts) < idle_ms {
            continue;
        }

        report.scanned += 1;
        let decision = gate::gate(&events, &meta, cfg);
        meta.gated_at = Some(chrono::Utc::now().to_rfc3339());
        let _ = std::fs::write(&meta_path, serde_json::to_string_pretty(&meta)?);

        if !decision.pass {
            tracing::debug!("harvest gate skipped session {}: {}", id, decision.reason);
            continue;
        }
        let steps = skeleton::commands_from_events(&events);
        if steps.is_empty() {
            continue; // nothing procedural to propose
        }
        // Bare tool markers carry no arguments — such a proposal says nothing a
        // reviewer could act on, redacted or not.
        if steps.iter().all(|s| s.starts_with("tool:")) {
            continue;
        }
        let duration_secs = match (events.first(), events.last()) {
            (Some(f), Some(l)) if l.timestamp >= f.timestamp => {
                ((l.timestamp - f.timestamp) / 1000) as i64
            }
            _ => 0,
        };
        let shape = gate::shape_gate(&steps, duration_secs, meta.marked, cfg);
        if !shape.pass {
            tracing::debug!(
                "harvest shape gate skipped session {}: {}",
                id,
                shape.reason
            );
            continue;
        }
        let skeleton = skeleton::skeletonize_steps(&steps);

        let similar_to = existing_workflow_steps
            .iter()
            .map(|(name, wsteps)| (name, proposal::step_similarity(&skeleton, wsteps)))
            .filter(|(_, sim)| *sim >= cfg.similarity_merge_threshold)
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(name, _)| name.clone());

        let title = proposal::clean_title(meta.title.as_deref(), &steps);
        // Project the session ran in (repo root), for project-local skill scope.
        let project = events
            .iter()
            .find_map(|e| e.working_dir.as_deref())
            .and_then(|wd| mur_common::project::project_id(std::path::Path::new(wd)));
        let p = proposal::Proposal {
            id: id.clone(),
            suggested_name: proposal::suggest_name(&title),
            title,
            steps,
            skeleton,
            event_count: events.len(),
            duration_secs,
            created_at: chrono::Utc::now().to_rfc3339(),
            status: proposal::ProposalStatus::Pending,
            similar_to,
            project,
        };
        proposal::save_in_dir(inbox_dir, &p)?;
        report.proposed += 1;
    }
    Ok(report)
}

/// Bind scan to the real `~/.mur` layout and the workflow store.
pub fn scan() -> Result<ScanReport> {
    let cfg = crate::store::config::load_config()?;
    if !cfg.harvest.auto_gate {
        return Ok(ScanReport {
            scanned: 0,
            proposed: 0,
        });
    }
    let recordings = crate::paths::mur_root(None)
        .join("session")
        .join("recordings");
    let store = crate::store::workflow_yaml::WorkflowYamlStore::default_store()?;
    let existing: Vec<(String, Vec<String>)> = store
        .list_all()
        .unwrap_or_default()
        .into_iter()
        .map(|w| {
            // Skeletonize so both sides of the similarity check speak the same
            // language — raw workflow commands never matched session skeletons.
            let steps: Vec<String> = w
                .steps
                .iter()
                .map(|s| s.command.clone().unwrap_or_else(|| s.description.clone()))
                .collect();
            (w.name.clone(), skeleton::skeletonize_steps(&steps))
        })
        .collect();
    scan_in_dirs(&recordings, &proposal::inbox_dir(), &existing, &cfg.harvest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{SessionEvent, SessionMeta};

    fn write_session(dir: &std::path::Path, id: &str, n_tools: usize, marked: bool) {
        std::fs::create_dir_all(dir).unwrap();
        let mut lines = vec![
            serde_json::to_string(&SessionEvent {
                timestamp: 1000,
                event_type: "user".into(),
                tool: None,
                content: "deploy the api".into(),
                ..Default::default()
            })
            .unwrap(),
        ];
        for i in 0..n_tools {
            lines.push(
                serde_json::to_string(&SessionEvent {
                    timestamp: 2000 + i as u64,
                    event_type: "tool_call".into(),
                    tool: Some("Bash".into()),
                    content: format!(r#"{{"command":"step-{} \"x\""}}"#, i),
                    exit_code: Some(0),
                    ..Default::default()
                })
                .unwrap(),
            );
        }
        std::fs::write(dir.join(format!("{}.jsonl", id)), lines.join("\n") + "\n").unwrap();
        let meta = SessionMeta {
            id: id.into(),
            source: "claude".into(),
            started_at: "2026-06-01T00:00:00Z".into(),
            stopped_at: None,
            title: Some("deploy the api".into()),
            tools_used: vec!["Bash".into()],
            user_turns: 3,
            assistant_turns: 3,
            marked,
            gated_at: None,
            harvested_at: None,
        };
        std::fs::write(
            dir.join(format!("{}.meta.json", id)),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn scan_stamps_project_from_repo_working_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let rec = tmp.path().join("recordings");
        let inbox = tmp.path().join("inbox");
        std::fs::create_dir_all(&rec).unwrap();

        let wd = repo.to_string_lossy().to_string();
        let mut lines = vec![
            serde_json::to_string(&SessionEvent {
                timestamp: 1000,
                event_type: "user".into(),
                content: "deploy the api".into(),
                working_dir: Some(wd.clone()),
                ..Default::default()
            })
            .unwrap(),
        ];
        for i in 0..4 {
            lines.push(
                serde_json::to_string(&SessionEvent {
                    timestamp: 2000 + i as u64,
                    event_type: "tool_call".into(),
                    tool: Some("Bash".into()),
                    content: format!(r#"{{"command":"step-{i} \"x\""}}"#),
                    exit_code: Some(0),
                    working_dir: Some(wd.clone()),
                    ..Default::default()
                })
                .unwrap(),
            );
        }
        std::fs::write(rec.join("s9.jsonl"), lines.join("\n") + "\n").unwrap();
        let meta = SessionMeta {
            id: "s9".into(),
            source: "claude".into(),
            started_at: "2026-06-01T00:00:00Z".into(),
            stopped_at: None,
            title: Some("deploy the api".into()),
            tools_used: vec!["Bash".into()],
            user_turns: 3,
            assistant_turns: 3,
            marked: false,
            gated_at: None,
            harvested_at: None,
        };
        std::fs::write(
            rec.join("s9.meta.json"),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .unwrap();

        let cfg = HarvestCfg {
            idle_minutes: 0,
            ..Default::default()
        };
        scan_in_dirs(&rec, &inbox, &[], &cfg).unwrap();
        let props = proposal::pending_in_dir(&inbox).unwrap();
        assert_eq!(props.len(), 1);
        // proposal.project == the session repo root → accept will stamp scope: Project
        assert_eq!(props[0].project, mur_common::project::project_id(&repo));
        assert!(props[0].project.is_some());
    }

    #[test]
    fn scan_proposes_once_and_marks_gated() {
        let tmp = tempfile::TempDir::new().unwrap();
        let rec = tmp.path().join("recordings");
        let inbox = tmp.path().join("inbox");
        write_session(&rec, "s1", 4, false);

        let cfg = HarvestCfg {
            idle_minutes: 0, // session timestamps are ancient → idle
            ..Default::default()
        };
        let r1 = scan_in_dirs(&rec, &inbox, &[], &cfg).unwrap();
        assert_eq!(r1.scanned, 1);
        assert_eq!(r1.proposed, 1);

        // Second scan: gated_at set → nothing scanned, nothing duplicated.
        let r2 = scan_in_dirs(&rec, &inbox, &[], &cfg).unwrap();
        assert_eq!(r2.scanned, 0);
        assert_eq!(proposal::pending_in_dir(&inbox).unwrap().len(), 1);
    }

    #[test]
    fn near_duplicate_gets_similar_to() {
        let tmp = tempfile::TempDir::new().unwrap();
        let rec = tmp.path().join("recordings");
        let inbox = tmp.path().join("inbox");
        write_session(&rec, "s2", 3, false);

        let cfg = HarvestCfg {
            idle_minutes: 0,
            ..Default::default()
        };
        let existing = vec![(
            "deploy-api".to_string(),
            vec![
                "step-0 <STR>".to_string(),
                "step-1 <STR>".to_string(),
                "step-2 <STR>".to_string(),
            ],
        )];
        scan_in_dirs(&rec, &inbox, &existing, &cfg).unwrap();
        let pending = proposal::pending_in_dir(&inbox).unwrap();
        assert_eq!(pending[0].similar_to.as_deref(), Some("deploy-api"));
    }
}
