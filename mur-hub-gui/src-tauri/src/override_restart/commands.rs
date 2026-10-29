//! The Tauri commands for the startup override (Hub spec §Command). Each
//! runs checks 1–4 on the async side, then moves the blocking job — the
//! native dialog, the stop, the ack wait — onto a blocking thread. Nothing
//! borrowed from the command frame crosses over: state is cloned out first.

use mur_common::agent::OverrideFlags;
use mur_core::cmd::agent::{CliLauncher, Launcher, Prepared, banner};
use tauri::{AppHandle, State, WebviewWindow};

use super::checks::{check_flags, check_name, check_window};
use super::dialog::confirm_override;
use super::jobs::{JobState, OverrideJobs};
use super::launcher::HubLauncher;
use super::{Ctx, NormalReply, OverrideReply, admit, run_normal, run_override};
use crate::SupervisorState;

/// The launcher a job uses: a service-installed agent restarts through its
/// service manager (`CliLauncher`); anything else may be the Hub's own child.
fn pick_launcher(
    supervisor: &mur_gui_core::sidecar::Supervisor,
    rt: &tokio::runtime::Handle,
    p: &Prepared,
) -> Box<dyn Launcher> {
    if p.has_service() {
        Box::new(CliLauncher::new(p.name(), p.home(), true))
    } else {
        Box::new(HubLauncher::new(
            supervisor.clone(),
            rt.clone(),
            p.name(),
            p.home(),
            false,
        ))
    }
}

fn joined<T>(name: &str, r: Result<Result<T, String>, tauri::Error>) -> Result<T, String> {
    r.map_err(|e| {
        format!("the override restart of {name} stopped unexpectedly ({e}); check its status")
    })?
}

#[tauri::command]
#[allow(clippy::too_many_arguments)] // the webview's invoke shape: one arg per flag
pub async fn restart_agent_with_overrides(
    app: AppHandle,
    window: WebviewWindow,
    jobs: State<'_, OverrideJobs>,
    supervisor: State<'_, SupervisorState>,
    name: String,
    no_hitl: bool,
    net_unrestricted: bool,
    full_perms: bool,
) -> Result<OverrideReply, String> {
    let flags = OverrideFlags {
        no_hitl,
        net_unrestricted,
        full_perms,
    };
    check_window(window.label())?;
    check_flags(flags)?;
    let home = crate::mur_home_path();
    check_name(&home, &name)?;
    let ctx = Ctx {
        home,
        name: name.clone(),
        window: window.label().to_owned(),
        jobs: jobs.inner().clone(),
    };
    let entry = admit(&ctx, JobState::Dialog)?;
    let supervisor = supervisor.0.clone();
    let rt = tauri::async_runtime::handle().inner().clone();
    let task = tauri::async_runtime::spawn_blocking(move || {
        run_override(
            ctx,
            entry,
            flags,
            &mut |p| confirm_override(&app, &window, p.name(), flags),
            &mut |p| pick_launcher(&supervisor, &rt, p),
        )
    });
    joined(&name, task.await)
}

#[tauri::command]
pub async fn restart_agent_normally(
    window: WebviewWindow,
    jobs: State<'_, OverrideJobs>,
    supervisor: State<'_, SupervisorState>,
    name: String,
) -> Result<NormalReply, String> {
    check_window(window.label())?;
    let home = crate::mur_home_path();
    check_name(&home, &name)?;
    let ctx = Ctx {
        home,
        name: name.clone(),
        window: window.label().to_owned(),
        jobs: jobs.inner().clone(),
    };
    let entry = admit(&ctx, JobState::Preparing)?;
    let supervisor = supervisor.0.clone();
    let rt = tauri::async_runtime::handle().inner().clone();
    let task = tauri::async_runtime::spawn_blocking(move || {
        run_normal(ctx, entry, &mut |p| pick_launcher(&supervisor, &rt, p))
    });
    joined(&name, task.await)
}

/// Read-only: the CLI's confirmation text, for the webview's own dialog.
#[tauri::command]
pub fn override_banner(
    name: String,
    no_hitl: bool,
    net_unrestricted: bool,
    full_perms: bool,
) -> Result<String, String> {
    let flags = OverrideFlags {
        no_hitl,
        net_unrestricted,
        full_perms,
    };
    check_flags(flags)?;
    check_name(&crate::mur_home_path(), &name)?;
    Ok(banner(&name, flags))
}
