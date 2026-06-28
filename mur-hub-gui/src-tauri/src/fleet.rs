//! Fleet management Tauri commands for MUR Hub.

use std::path::PathBuf;

use mur_common::fleet::Job;
use mur_core::cmd::fleet::{control, create, delete, export, import, jobs, roster, run, store};
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
pub struct FleetDetail {
    pub name: String,
    pub display_name: String,
    pub goal: String,
    pub router: String,
    pub members: Vec<String>,
    pub channel_id: String,
    pub stopped: bool,
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
    Ok(FleetDetail {
        name: fleet.name.clone(),
        display_name: display(&fleet.name, &fleet.display_name),
        goal: fleet.goal.clone(),
        router: fleet.router_or_concierge().to_string(),
        members: fleet.members.clone(),
        channel_id: fleet.channel_id.clone(),
        stopped,
    })
}

#[tauri::command]
pub fn fleet_create(
    name: String,
    members: Vec<String>,
    router: Option<String>,
    goal: String,
) -> Result<(), String> {
    let home = mur_home_path();
    create::cmd_fleet_create(&home, &name, members, router, Some(goal))
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
pub async fn fleet_run(name: String, app: tauri::AppHandle) -> Result<(), String> {
    let home = mur_home_path();
    let fleet_name = name.clone();
    // cmd_fleet_run is async but does blocking I/O internally (UnixStream dial).
    // Use spawn_blocking with a dedicated runtime so tokio worker threads aren't tied up.
    tokio::task::spawn_blocking(move || {
        let ok = tokio::runtime::Runtime::new()
            .expect("fleet run runtime")
            .block_on(run::cmd_fleet_run(&home, &fleet_name, None))
            .is_ok();
        let _ = app.emit(
            "fleet:run_done",
            serde_json::json!({ "name": fleet_name, "ok": ok }),
        );
    });
    Ok(())
}

#[tauri::command]
pub fn fleet_send(name: String, text: String) -> Result<String, String> {
    let home = mur_home_path();
    let job = jobs::enqueue_job(&home, &name, &text, "hub").map_err(|e| e.to_string())?;
    Ok(job.id)
}

#[tauri::command]
pub fn fleet_jobs(name: String, all: bool) -> Result<Vec<JobRow>, String> {
    let home = mur_home_path();
    let job_list = jobs::list_jobs(&home, &name).map_err(|e| e.to_string())?;
    let filtered: Vec<_> = if all {
        job_list
    } else {
        job_list.into_iter().filter(|j| !j.status.is_terminal()).collect()
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
        import::ImportOpts { force: false, no_members: false, yes: true },
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
        assert_eq!(job_to_row(make_job("a", JobStatus::Running)).status, "running");
        assert_eq!(job_to_row(make_job("b", JobStatus::Done)).status, "done");
        assert_eq!(job_to_row(make_job("c", JobStatus::Failed)).status, "failed");
        assert_eq!(job_to_row(make_job("d", JobStatus::Queued)).status, "queued");
        assert_eq!(job_to_row(make_job("e", JobStatus::Canceled)).status, "canceled");
    }

    #[test]
    fn display_falls_back_to_name() {
        assert_eq!(display("dev", ""), "dev");
        assert_eq!(display("dev", "Dev Squad"), "Dev Squad");
    }
}
