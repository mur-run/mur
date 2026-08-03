//! Fleet management Tauri commands for MUR Hub.

use std::path::PathBuf;

use mur_common::fleet::Job;
use mur_common::parallel::ParallelConfig;
use mur_core::cmd::fleet::{
    control, create, delete, export, import, jobs, loop_run, roster, run, settings, store,
};
use serde::Serialize;
use tauri::Emitter;

use crate::mur_home_path;

#[derive(Serialize, Clone)]
pub struct FleetSummary {
    pub name: String,
    pub display_name: String,
    pub goal: String,
    pub member_count: usize,
    pub active_jobs: usize,
    pub stopped: bool,
    pub running: bool,
}

#[derive(Serialize, Clone)]
pub struct FleetLoopView {
    pub trigger: String,
    pub max_iterations: u32,
    pub budget_usd: f64,
    pub deadline: String,
    pub done_when: String,
    pub last_run: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct ParallelSummaryView {
    pub mode: String,
    pub track_count: usize,
    pub target_file: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct FleetDetail {
    pub name: String,
    pub display_name: String,
    pub goal: String,
    pub router: String,
    pub members: Vec<String>,
    pub channel_id: String,
    pub stopped: bool,
    pub loop_cfg: Option<FleetLoopView>,
    pub parallel_summary: Option<ParallelSummaryView>,
}

fn parallel_summary_view(cfg: &ParallelConfig) -> ParallelSummaryView {
    ParallelSummaryView {
        mode: match cfg.mode {
            mur_common::parallel::ParallelMode::Speculative => "speculative".to_string(),
            mur_common::parallel::ParallelMode::Partition => "partition".to_string(),
        },
        track_count: cfg.tracks.len(),
        target_file: cfg.partition.as_ref().map(|p| p.target_file.clone()),
    }
}

/// Read fleet's `.last_run` auto-run sentinel (unix seconds, written by
/// `mur-daemon`'s `fleet_tick`) format it RFC3339, if present.
fn read_last_run_rfc3339(mur_home: &std::path::Path, name: &str) -> Option<String> {
    let secs: i64 = std::fs::read_to_string(store::fleet_dir(mur_home, name).join(".last_run"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    chrono::DateTime::from_timestamp(secs, 0).map(|dt| dt.to_rfc3339())
}

#[derive(Serialize, Clone)]
pub struct JobRow {
    pub id: String,
    pub text: String,
    pub source: String,
    pub status: String,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub run_id: Option<String>,
    pub result: Option<String>,
    pub error: Option<String>,
}

fn job_to_row(job: Job) -> JobRow {
    JobRow {
        id: job.id,
        text: job.text,
        source: job.source,
        status: job.status.as_str().to_string(),
        created_at: job.created_at,
        started_at: job.started_at,
        finished_at: job.finished_at,
        run_id: job.run_id,
        result: job.result,
        error: job.error,
    }
}

fn display(name: &str, display_name: &str) -> String {
    if display_name.is_empty() {
        name.to_string()
    } else {
        display_name.to_string()
    }
}

// ── Tauri commands ────────────────────────────────────────────────────────────

#[tauri::command]
pub fn fleet_list() -> Result<Vec<FleetSummary>, String> {
    let home = mur_home_path();
    let names = store::list_fleets(&home).map_err(|e| e.to_string())?;
    let mut summaries = Vec::new();
    for name in names {
        if let Ok(fleet) = store::load_fleet(&home, &name) {
            let active_jobs = jobs::list_jobs(&home, &name)
                .unwrap_or_default()
                .iter()
                .filter(|j| !j.status.is_terminal())
                .count();
            let stopped = control::is_stopped(&home, &name);
            summaries.push(FleetSummary {
                name: fleet.name.clone(),
                display_name: display(&fleet.name, &fleet.display_name),
                goal: fleet.goal.clone(),
                member_count: fleet.members.len(),
                active_jobs,
                stopped,
                running: active_jobs > 0 && !stopped,
            });
        }
    }
    Ok(summaries)
}

#[tauri::command]
pub fn fleet_detail(name: String) -> Result<FleetDetail, String> {
    let home = mur_home_path();
    let fleet = store::load_fleet(&home, &name).map_err(|e| e.to_string())?;
    let stopped = control::is_stopped(&home, &name);
    let loop_cfg = fleet.loop_cfg.as_ref().map(|l| FleetLoopView {
        trigger: l.trigger.clone(),
        max_iterations: l.max_iterations,
        budget_usd: l.budget_usd,
        deadline: l.deadline.clone(),
        done_when: l.done_when.clone(),
        last_run: read_last_run_rfc3339(&home, &name),
    });
    let parallel_summary = fleet.parallel.as_ref().map(parallel_summary_view);
    Ok(FleetDetail {
        name: fleet.name.clone(),
        display_name: display(&fleet.name, &fleet.display_name),
        goal: fleet.goal.clone(),
        router: fleet.router_or_concierge().to_string(),
        members: fleet.members.clone(),
        channel_id: fleet.channel_id.clone(),
        stopped,
        loop_cfg,
        parallel_summary,
    })
}

#[tauri::command]
pub fn fleet_create(
    name: String,
    members: Vec<String>,
    router: Option<String>,
    goal: String,
    parallel: Option<ParallelConfig>,
) -> Result<(), String> {
    let home = mur_home_path();
    create::cmd_fleet_create(&home, &name, members, router, Some(goal), parallel)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn fleet_delete(name: String) -> Result<(), String> {
    let home = mur_home_path();
    // yes: true — Hub already confirmed user via JS confirm()
    delete::cmd_fleet_delete(&home, &name, true).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn fleet_stop(name: String) -> Result<(), String> {
    let home = mur_home_path();
    control::cmd_fleet_stop(&home, &name).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn fleet_start(name: String) -> Result<(), String> {
    let home = mur_home_path();
    control::cmd_fleet_start(&home, &name).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn fleet_run(name: String, worktree: bool, app: tauri::AppHandle) -> Result<(), String> {
    let home = mur_home_path();
    let fleet_name = name.clone();
    // cmd_fleet_run is async but does blocking I/O internally (UnixStream dial).
    // Use spawn_blocking with a dedicated runtime so tokio worker threads aren't tied up.
    tokio::task::spawn_blocking(move || {
        let ok = tokio::runtime::Runtime::new()
            .expect("fleet run runtime")
            .block_on(run::cmd_fleet_run(&home, &fleet_name, None, worktree))
            .is_ok();
        let _ = app.emit(
            "fleet:run_done",
            serde_json::json!({ "name": fleet_name, "ok": ok }),
        );
    });
    Ok(())
}

#[tauri::command]
pub async fn fleet_run_loop(
    name: String,
    max_iterations: Option<u32>,
    deadline: Option<String>,
    budget_usd: Option<f64>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let home = mur_home_path();
    let fleet_name = name.clone();
    tokio::task::spawn_blocking(move || {
        let ok = tokio::runtime::Runtime::new()
            .expect("fleet run loop runtime")
            .block_on(loop_run::cmd_fleet_run_loop(
                &home,
                &fleet_name,
                max_iterations,
                deadline,
                budget_usd,
            ))
            .is_ok();
        let _ = app.emit(
            "fleet:run_done",
            serde_json::json!({ "name": fleet_name, "ok": ok }),
        );
    });
    Ok(())
}

#[tauri::command]
pub fn fleet_set_loop(
    name: String,
    trigger: Option<String>,
    max_iterations: Option<u32>,
    deadline: Option<String>,
    budget_usd: Option<f64>,
    done_when: Option<String>,
) -> Result<(), String> {
    let home = mur_home_path();
    settings::cmd_fleet_set_loop(
        &home,
        &name,
        trigger,
        max_iterations,
        deadline,
        budget_usd,
        done_when,
    )
    .map_err(|e| e.to_string())
}

/// The next `count` fire times for a 5-field cron expression, formatted in the
/// machine's local time.
///
/// Deliberately routed through `mur_agent_runtime::scheduler` rather than a
/// JavaScript cron library: the daemon decides due-ness with this same parser,
/// and a preview that disagrees with the scheduler (on six-field padding, or
/// day-of-week numbering) is worse than no preview at all.
///
/// `Err` means the expression does not parse. `Ok(vec![])` means it parses but
/// will never fire again — two different problems, and the caller shows two
/// different messages.
#[tauri::command]
pub fn cron_preview(expr: String, count: usize) -> Result<Vec<String>, String> {
    let fires = mur_agent_runtime::scheduler::next_n_fires(expr.trim(), count)
        .map_err(|e| e.to_string())?;
    Ok(fires
        .iter()
        .map(|t| t.format("%-m/%-d %H:%M").to_string())
        .collect())
}

#[tauri::command]
pub fn get_fleet_autorun() -> Result<bool, String> {
    let cfg = mur_core::store::config::load_config().map_err(|e| e.to_string())?;
    Ok(cfg.fleet.autorun)
}

#[tauri::command]
pub fn set_fleet_autorun(enabled: bool) -> Result<(), String> {
    let mut cfg = mur_core::store::config::load_config().map_err(|e| e.to_string())?;
    cfg.fleet.autorun = enabled;
    mur_core::store::config::save_config(&cfg).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn fleet_send(name: String, text: String) -> Result<String, String> {
    let home = mur_home_path();
    let job = jobs::enqueue_job(&home, &name, &text, "hub").map_err(|e| e.to_string())?;
    Ok(job.id)
}

/// Cancel a queued job. Queued-only — see `jobs::cmd_fleet_cancel`.
#[tauri::command]
pub fn fleet_cancel_job(name: String, id: String) -> Result<(), String> {
    let home = mur_home_path();
    // yes: true — Hub already confirmed user via JS confirm()
    jobs::cmd_fleet_cancel(&home, &name, &id, true).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn fleet_jobs(name: String, all: bool) -> Result<Vec<JobRow>, String> {
    let home = mur_home_path();
    let job_list = jobs::list_jobs(&home, &name).map_err(|e| e.to_string())?;
    let filtered: Vec<_> = if all {
        job_list
    } else {
        job_list
            .into_iter()
            .filter(|j| !j.status.is_terminal())
            .collect()
    };
    Ok(filtered.into_iter().map(job_to_row).collect())
}

#[tauri::command]
pub fn fleet_add_member(name: String, agent: String) -> Result<(), String> {
    let home = mur_home_path();
    roster::cmd_fleet_add(&home, &name, vec![agent]).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn fleet_remove_member(name: String, agent: String) -> Result<(), String> {
    let home = mur_home_path();
    roster::cmd_fleet_remove(&home, &name, vec![agent]).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn fleet_export_to(name: String, path: String) -> Result<(), String> {
    let home = mur_home_path();
    let out_path = PathBuf::from(&path);
    let now = chrono::Utc::now().to_rfc3339();
    export::cmd_fleet_export(&home, &name, false, Some(out_path), &now).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn fleet_export(name: String) -> Result<String, String> {
    let home = mur_home_path();
    let out_path = if let Some(desktop) = dirs::desktop_dir().filter(|d| d.exists()) {
        desktop.join(format!("{name}.fleet"))
    } else {
        let fallback = home.join("exports");
        std::fs::create_dir_all(&fallback).map_err(|e| e.to_string())?;
        fallback.join(format!("{name}.fleet"))
    };
    let now = chrono::Utc::now().to_rfc3339();
    export::cmd_fleet_export(&home, &name, false, Some(out_path.clone()), &now)
        .map_err(|e| e.to_string())?;
    Ok(out_path.to_string_lossy().to_string())
}

#[tauri::command]
pub fn fleet_import(path: String) -> Result<String, String> {
    let home = mur_home_path();
    let file = PathBuf::from(&path);
    // Extract fleet name from filename stem: "dev-squad.fleet" → "dev-squad"
    let fleet_name = file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    import::cmd_fleet_import(
        &home,
        &file,
        import::ImportOpts {
            force: false,
            no_members: false,
            yes: true,
        },
    )
    .map_err(|e| e.to_string())?;
    Ok(fleet_name)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::fleet::{Job, JobStatus};

    fn make_job(id: &str, status: JobStatus) -> Job {
        Job {
            id: id.to_string(),
            text: "test".to_string(),
            source: "cli".to_string(),
            status,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            started_at: None,
            finished_at: None,
            run_id: None,
            result: None,
            error: None,
        }
    }

    #[test]
    fn job_to_row_maps_status_to_string() {
        assert_eq!(
            job_to_row(make_job("a", JobStatus::Running)).status,
            "running"
        );
        assert_eq!(job_to_row(make_job("b", JobStatus::Done)).status, "done");
        assert_eq!(
            job_to_row(make_job("c", JobStatus::Failed)).status,
            "failed"
        );
        assert_eq!(
            job_to_row(make_job("d", JobStatus::Queued)).status,
            "queued"
        );
        assert_eq!(
            job_to_row(make_job("e", JobStatus::Canceled)).status,
            "canceled"
        );
    }

    #[test]
    fn display_falls_back_to_name() {
        assert_eq!(display("dev", ""), "dev");
        assert_eq!(display("dev", "Dev Squad"), "Dev Squad");
    }

    #[test]
    fn parallel_summary_view_speculative_and_partition() {
        use mur_common::parallel::{
            JudgeConfig, ParallelConfig, ParallelMode, PartitionConfig, TrackConfig,
        };

        let spec = ParallelConfig {
            mode: ParallelMode::Speculative,
            tracks: vec![
                TrackConfig {
                    name: "a".into(),
                    approach: "x".into(),
                    model: None,
                },
                TrackConfig {
                    name: "b".into(),
                    approach: "y".into(),
                    model: None,
                },
            ],
            judge: JudgeConfig {
                model: "claude-opus-4-8".into(),
                rubric: Default::default(),
            },
            pre_filter: vec![],
            partition: None,
        };
        let view = parallel_summary_view(&spec);
        assert_eq!(view.mode, "speculative");
        assert_eq!(view.track_count, 2);
        assert_eq!(view.target_file, None);

        let part = ParallelConfig {
            mode: ParallelMode::Partition,
            tracks: vec![],
            judge: JudgeConfig {
                model: "claude-opus-4-8".into(),
                rubric: Default::default(),
            },
            pre_filter: vec![],
            partition: Some(PartitionConfig {
                target_file: "src/widget.rs".into(),
            }),
        };
        let view2 = parallel_summary_view(&part);
        assert_eq!(view2.mode, "partition");
        assert_eq!(view2.track_count, 0);
        assert_eq!(view2.target_file.as_deref(), Some("src/widget.rs"));
    }

    #[test]
    fn last_run_reads_sentinel_and_handles_absence() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let fleet_dir = store::fleet_dir(home, "dev");
        std::fs::create_dir_all(&fleet_dir).unwrap();

        assert_eq!(read_last_run_rfc3339(home, "dev"), None);

        std::fs::write(fleet_dir.join(".last_run"), "1751328000").unwrap();
        let got = read_last_run_rfc3339(home, "dev").unwrap();
        // RFC3339-parseable and round-trips to the same unix timestamp (avoids
        // hardcoding a guessed calendar year, which would be a flaky assertion).
        let parsed = chrono::DateTime::parse_from_rfc3339(&got).unwrap();
        assert_eq!(parsed.timestamp(), 1751328000);
    }
}
