//! Quitting the Hub with an override restart in flight (Hub spec §Quit).
//!
//! A job past publish has a live override file the next boot of that agent
//! would take, so quit must not just vanish: it asks once, and on "Quit
//! anyway" the quit path claims every published file and withdraws it (or
//! classifies what a boot already did with it). Jobs still preparing see
//! `is_quitting` at their publish gate and publish nothing. An updater
//! restart ignores `prevent_exit`, so it claims synchronously, unasked.

use std::path::Path;

use mur_core::cmd::agent::{outcome_text, withdraw_on_quit};
use tauri::{AppHandle, ExitRequestApi, Manager, RunEvent};
use tracing::info;

use super::checks::DASHBOARD_LABEL;
use super::dialog::{self, QUIT_LABEL};
use super::jobs::{OverrideJobs, QuitClaim};

const QUIT_TITLE: &str = "Quit MUR Hub?";

/// The app's run-loop hook; only `ExitRequested` matters here.
pub fn on_run_event(app: &AppHandle, event: &RunEvent) {
    if let RunEvent::ExitRequested { code, api, .. } = event {
        on_exit_requested(app, *code, api);
    }
}

/// `RunEvent::ExitRequested`. Returns at once; the prompt runs on its own
/// thread because `dialog::ask` blocks.
pub fn on_exit_requested(app: &AppHandle, code: Option<i32>, api: &ExitRequestApi) {
    let Some(jobs) = app.try_state::<OverrideJobs>().map(|s| s.inner().clone()) else {
        return;
    };
    if jobs.is_quitting() || jobs.active_names().is_empty() {
        return; // our own app.exit(0) coming back round, or nothing to protect
    }
    let home = crate::mur_home_path();
    if code == Some(tauri::RESTART_EXIT_CODE) {
        run_claims(&home, &jobs, jobs.begin_quit());
        return;
    }
    api.prevent_exit();
    prompt_and_quit(app, &jobs, &home);
}

/// Menu/tray Quit entry. `app.exit(0)` and the native `terminate:` selector
/// both skip `ExitRequested`, so those entries call this instead: it asks once
/// when a restart is in flight, otherwise exits straight away.
///
/// Known gap: the Dock's right-click Quit sends `terminate:` and tao's
/// delegate implements only `applicationWillTerminate:` (already decided, not
/// cancellable), so it bypasses this guard, as do force-quit and logout. A
/// stale override file left that way is rejected as `stale` on the next boot.
pub fn request_quit(app: &AppHandle) {
    let jobs = app.try_state::<OverrideJobs>().map(|s| s.inner().clone());
    match quit_action(jobs.as_ref()) {
        QuitAction::ExitNow => app.exit(0),
        QuitAction::Prompt => {
            if let Some(jobs) = jobs {
                prompt_and_quit(app, &jobs, &crate::mur_home_path());
            }
        }
    }
}

/// What a menu/tray Quit does.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum QuitAction {
    ExitNow,
    Prompt,
}

/// Prompt only while a restart is in flight and no quit is already underway.
pub(super) fn quit_action(jobs: Option<&OverrideJobs>) -> QuitAction {
    match jobs {
        Some(j) if !j.is_quitting() && !j.active_names().is_empty() => QuitAction::Prompt,
        _ => QuitAction::ExitNow,
    }
}

fn prompt_and_quit(app: &AppHandle, jobs: &OverrideJobs, home: &Path) {
    if !jobs.begin_quit_prompt() {
        return;
    }
    let (app, jobs, home) = (app.clone(), jobs.clone(), home.to_path_buf());
    std::thread::spawn(move || {
        let body = quit_body(&jobs.active_names());
        let dashboard = app.get_webview_window(DASHBOARD_LABEL);
        let quit = dialog::ask(&app, dashboard.as_ref(), QUIT_TITLE, &body, QUIT_LABEL);
        jobs.end_quit_prompt();
        if quit {
            run_claims(&home, &jobs, jobs.begin_quit());
            app.exit(0);
        }
    });
}

fn quit_body(names: &[String]) -> String {
    format!(
        "An override restart of {} is in progress. Quit anyway?",
        names.join(", ")
    )
}

/// Withdraw (or classify) each claimed file, then free its job slot.
pub fn run_claims(home: &Path, jobs: &OverrideJobs, claims: Vec<QuitClaim>) {
    for c in claims {
        let outcome = withdraw_on_quit(home, &c.slug, &c.nonce, &c.window);
        info!(agent = %c.slug, "quit: {}", outcome_text(&c.slug, &outcome));
        jobs.finish_claim(&c.slug);
    }
}

#[cfg(all(test, unix))]
#[path = "quit_tests.rs"]
mod tests;
