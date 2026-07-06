//! Panel P2 data commands: thin adapters over mur-core lib fns.
//! Spec: docs/superpowers/specs/2026-07-06-murmur-panel-p2-data-tabs-design.md

use serde::Serialize;

use crate::mur_home_path;

#[tauri::command]
pub fn panel_schedule_status(agent: Option<String>) -> mur_core::schedule_status::ScheduleStatus {
    mur_core::schedule_status::schedule_status(&mur_home_path(), agent.as_deref())
}

#[tauri::command]
pub fn panel_cost(agent: String) -> mur_core::cmd::agent::stats::TokenTotals {
    if agent.contains('/') || agent.contains('\\') || agent.contains("..") {
        return mur_core::cmd::agent::stats::TokenTotals::default();
    }
    mur_core::cmd::agent::stats::agent_token_totals(&mur_home_path().join("agents").join(agent))
}

#[tauri::command]
pub fn panel_proposals() -> Vec<mur_core::harvest::proposal::ProposalSummary> {
    mur_core::harvest::proposal::list_pending(&mur_home_path(), 5)
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GitInfo {
    pub root: Option<String>,
    pub branch: Option<String>,
    pub branches: Vec<String>,
    pub worktrees: Vec<String>,
    pub dirty: bool,
}

/// Channels the agent participates in + its pending HITL gates, for the
/// Activities tab.
#[derive(Debug, Clone, Serialize)]
pub struct Activities {
    pub channels: Vec<crate::work::ChannelSummary>,
    pub hitl: Vec<crate::hitl::HitlRequestView>,
}

#[tauri::command]
pub async fn panel_activities(agent: String) -> Activities {
    let home = mur_home_path();
    let channels = tokio::task::spawn_blocking(move || crate::work::list_channels(&home))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default()
        .into_iter()
        .filter(|c| {
            c.participants
                .iter()
                .any(|p| p.id.eq_ignore_ascii_case(&agent))
        })
        .collect();
    let hitl = crate::hitl::pending_views()
        .into_iter()
        .filter(|h| h.agent.eq_ignore_ascii_case(&agent))
        .collect();
    Activities { channels, hitl }
}

/// Best-effort `git` shell-out; returns an empty `GitInfo` for non-repos.
#[tauri::command]
pub fn panel_git_info(cwd: String) -> GitInfo {
    fn git(cwd: &str, args: &[&str]) -> Option<String> {
        let out = std::process::Command::new("git")
            .current_dir(cwd)
            .args(args)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    let Some(root) = git(&cwd, &["rev-parse", "--show-toplevel"]) else {
        return GitInfo::default();
    };

    GitInfo {
        branch: git(&cwd, &["rev-parse", "--abbrev-ref", "HEAD"]),
        branches: git(
            &cwd,
            &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
        )
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default(),
        worktrees: git(&cwd, &["worktree", "list", "--porcelain"])
            .map(|s| {
                s.lines()
                    .filter_map(|l| l.strip_prefix("worktree "))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        dirty: git(&cwd, &["status", "--porcelain"]).is_some_and(|s| !s.is_empty()),
        root: Some(root),
    }
}
