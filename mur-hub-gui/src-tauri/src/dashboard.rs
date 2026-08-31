//! Opening the web Dashboard from the Hub.
//!
//! The Dashboard is a separate surface served by `mur daemon serve` on 3847.
//! The Hub does not run it, so a plain link would point at a port that is
//! usually closed — and a link into a dead port is worse than no link.
//!
//! **The check identifies MUR, not the port.** A TCP connect is not enough and
//! neither is a 200: that server answers 200 `text/html` with the SPA for any
//! unmatched path, and an unrelated process can hold 3847. The check is that
//! `/api/v1/health` answers, parses as JSON, and reports `status: "ok"`.
//!
//! **Whoever starts it, stops it.** A server the Hub started is killed when the
//! Hub quits; one that was already running is left alone, because someone else
//! owns it.
//!
//! See `docs/superpowers/specs/2026-08-30-hub-dashboard-surface-split-design.md`, D5.

use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::AppHandle;
use tauri_plugin_shell::ShellExt;
use tracing::{info, warn};

const PORT: u16 = 3847;
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// How long to wait for a server we just started to answer.
const STARTUP_BUDGET: Duration = Duration::from_secs(10);
const STARTUP_POLL: Duration = Duration::from_millis(200);

/// The server the Hub started, if it started one. Never holds a server that was
/// already running — that one belongs to whoever started it.
static OWNED: OnceLock<Mutex<Option<Child>>> = OnceLock::new();

fn owned() -> &'static Mutex<Option<Child>> {
    OWNED.get_or_init(|| Mutex::new(None))
}

fn base_url() -> String {
    format!("http://localhost:{PORT}")
}

/// What is on the port.
#[derive(Debug, PartialEq, Eq)]
enum Probe {
    /// Health answered as MUR.
    Mur,
    /// Nothing is listening.
    Closed,
    /// Something answered, but it is not MUR. The string says what was seen —
    /// the one thing the user needs in order to act on it.
    Foreign(String),
}

/// Classify a health response body. Split out from the request so the rule that
/// matters — "answers, parses as JSON, says ok" — is testable without a socket.
fn classify(body: &str) -> Probe {
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(v) if v.get("status").and_then(|s| s.as_str()) == Some("ok") => Probe::Mur,
        Ok(_) => Probe::Foreign(format!(
            "answered JSON, but not MUR's health: {}",
            body.chars().take(120).collect::<String>()
        )),
        Err(_) => Probe::Foreign(format!(
            "answered, but not with JSON: {}",
            body.chars()
                .take(120)
                .collect::<String>()
                .replace('\n', " ")
        )),
    }
}

fn probe() -> Probe {
    let client = match reqwest::blocking::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(e) => return Probe::Foreign(format!("could not build an HTTP client: {e}")),
    };
    match client.get(format!("{}/api/v1/health", base_url())).send() {
        Err(e) if e.is_connect() => Probe::Closed,
        Err(e) => Probe::Foreign(format!("did not answer a health request: {e}")),
        Ok(r) => match r.text() {
            Ok(body) => classify(&body),
            Err(e) => Probe::Foreign(format!("answered with an unreadable body: {e}")),
        },
    }
}

/// Result of asking for the Dashboard: the URL that was opened, and whether the
/// Hub had to start the server to get there.
#[derive(Debug, Serialize)]
pub struct Opened {
    pub url: String,
    pub started: bool,
}

/// Open the Dashboard, starting the local server first if nothing is on the
/// port. Refuses — rather than opening a browser onto a stranger's server — if
/// something that is not MUR is listening.
#[tauri::command]
pub async fn dashboard_open(app: AppHandle) -> Result<Opened, String> {
    // The probe and the startup wait are blocking, and can take seconds. Off
    // the async runtime they cannot stall anything else the Hub is doing —
    // which is half of what makes the click show "starting…" instead of
    // freezing.
    let started = tauri::async_runtime::spawn_blocking(ensure_server)
        .await
        .map_err(|e| format!("The Dashboard check did not finish: {e}"))??;
    // ponytail: shell().open is deprecated in favour of tauri-plugin-opener,
    // which the Hub does not carry; one call does not justify a new plugin.
    // Swap if that plugin arrives for another reason.
    #[allow(deprecated)]
    app.shell()
        .open(base_url(), None)
        .map_err(|e| format!("Could not open a browser: {e}"))?;
    Ok(Opened {
        url: base_url(),
        started,
    })
}

/// Make sure MUR is on the port, starting it if nothing is. `true` when the
/// Hub started it — which is also what makes the Hub responsible for stopping
/// it.
fn ensure_server() -> Result<bool, String> {
    match probe() {
        Probe::Mur => Ok(false),
        Probe::Foreign(what) => Err(format!(
            "Port {PORT} is held by something that is not MUR — it {what}. \
             Not opening a browser onto it."
        )),
        Probe::Closed => start_server().map(|()| true),
    }
}

/// Spawn `mur daemon serve` and wait for it to answer as MUR. A server that
/// starts but never answers is killed rather than left behind: the Hub would
/// otherwise own a process it cannot use and the user cannot see.
fn start_server() -> Result<(), String> {
    let mur = crate::cli_tools::resolve_mur()
        .ok_or("The `mur` CLI was not found, so the Dashboard server cannot be started.")?;
    let child = Command::new(&mur)
        .args(["daemon", "serve", "--port", &PORT.to_string()])
        .env("MUR_HOME", crate::mur_home_path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Could not start `mur daemon serve`: {e}"))?;
    info!(port = PORT, "dashboard: started the local server");
    *owned().lock().unwrap() = Some(child);

    let deadline = Instant::now() + STARTUP_BUDGET;
    while Instant::now() < deadline {
        match probe() {
            Probe::Mur => return Ok(()),
            // Something else grabbed the port in the gap. Ours is useless now.
            Probe::Foreign(what) => {
                shutdown();
                return Err(format!("Port {PORT} was taken while starting — it {what}."));
            }
            Probe::Closed => std::thread::sleep(STARTUP_POLL),
        }
    }
    shutdown();
    Err(format!(
        "The Dashboard server did not answer within {}s.",
        STARTUP_BUDGET.as_secs()
    ))
}

/// Stop the server if the Hub started it. Idempotent; safe to call on exit.
pub fn shutdown() {
    let Ok(mut guard) = owned().lock() else {
        return;
    };
    if let Some(mut child) = guard.take() {
        if let Err(e) = child.kill() {
            warn!(error = %e, "dashboard: could not stop the server we started");
        }
        let _ = child.wait();
        info!("dashboard: stopped the server we started");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The failure this check exists to prevent: the server answers 200 with
    /// the SPA for any unmatched path, so a status code proves nothing. Only
    /// the body can say whether MUR is on the port.
    #[test]
    fn an_html_answer_is_not_mur() {
        let spa = "<!doctype html>\n<html><head><title>MUR</title></head></html>";
        assert!(matches!(classify(spa), Probe::Foreign(_)));
    }

    #[test]
    fn murs_health_is_recognised() {
        assert_eq!(
            classify(r#"{"status":"ok","version":"2.71.9","source":"local"}"#),
            Probe::Mur
        );
    }

    /// A JSON API on the same port is still not MUR. "It parsed" is not the
    /// test; "it said ok" is.
    #[test]
    fn another_json_service_is_not_mur() {
        assert!(matches!(
            classify(r#"{"service":"something-else","healthy":true}"#),
            Probe::Foreign(_)
        ));
        assert!(matches!(
            classify(r#"{"status":"degraded"}"#),
            Probe::Foreign(_)
        ));
    }

    /// The message has to name what was seen — "not MUR" alone leaves the user
    /// with nothing to act on.
    #[test]
    fn the_refusal_says_what_answered() {
        let Probe::Foreign(what) = classify("Grafana") else {
            panic!("expected foreign")
        };
        assert!(what.contains("Grafana"), "{what}");
    }
}
