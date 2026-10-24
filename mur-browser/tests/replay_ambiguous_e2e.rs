//! Browser-backed lock: a `click` whose recorded locator matches two elements
//! still clicks the first one (replay behaviour is unchanged), and the report
//! names the ambiguity in a warning without changing the verdict.
//!
//! Opt-in: set `MUR_BROWSER_E2E=1`. Everything else skips, because this needs
//! `node` plus the pinned `@playwright/mcp` install and its Chromium (both from
//! `mur browser setup`).
//!
//! Inside a sandboxed host, pass Chromium `--no-sandbox` through
//! `MUR_BROWSER_CHROMIUM_ARGS`; replay forwards it to the launch untouched.

use mur_browser::recorder::{Action, Mode, Run, Step};
use mur_browser::replay::{ReplayOptions, replay_live};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const GATE: &str = "MUR_BROWSER_E2E";

fn gated() -> bool {
    if std::env::var_os(GATE).is_some_and(|v| !v.is_empty() && v != "0") {
        return true;
    }
    eprintln!("skipped: set {GATE}=1 to run the replay ambiguous-locator e2e");
    false
}

/// Two buttons share the role and name the recording points at. Clicking one
/// writes which into the page, so the assert step proves which was clicked.
const PAGE: &str = "<!doctype html><title>t</title>\
    <button onclick=\"document.getElementById('out').textContent='clicked first'\">Save</button>\
    <button onclick=\"document.getElementById('out').textContent='clicked second'\">Save</button>\
    <p id=\"out\">nothing clicked</p>";

/// Serve `PAGE` on 127.0.0.1 (replay's guard only allows http(s) URLs).
async fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    PAGE.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(PAGE.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}/")
}

fn step(n: u32, action: Action, value: Option<&str>, locators: &[&str]) -> Step {
    Step {
        step: n,
        intent: "e2e step".to_string(),
        intent_auto: false,
        action,
        value: value.map(str::to_string),
        locators: locators.iter().map(|l| l.to_string()).collect(),
        healed: false,
        last_hit: 0,
        ref_at_record: None,
    }
}

#[tokio::test]
async fn ambiguous_click_hits_the_first_match_and_warns() {
    if !gated() {
        return;
    }
    let url = serve().await;
    let run = Run {
        name: "ambiguous-click-e2e".to_string(),
        mode: Mode::Test,
        profile: None,
        recorded_at: chrono::Utc::now(),
        description: None,
        tags: vec![],
        replayed_at: None,
        replay_count: 0,
        steps: vec![
            step(1, Action::Goto, Some(&url), &[]),
            step(2, Action::Click, None, &["role:button[name=\"Save\"]"]),
            step(3, Action::AssertText, Some("clicked first"), &[]),
        ],
    };
    let report = replay_live(&run, &[], ReplayOptions::default(), None, None)
        .await
        .expect("replay ran");

    // Unchanged behaviour: the first match was clicked and the run is green.
    assert_eq!((report.passed, report.failed), (3, 0), "{report:#?}");
    assert_eq!(report.verdict(), "green");

    // New: the click step names the ambiguity; no other step warns.
    let warning = report.steps[1]
        .warning
        .as_deref()
        .unwrap_or_else(|| panic!("click step carries a warning: {report:#?}"));
    assert!(
        warning
            .starts_with("role:button[name=\"Save\"] matched 2 elements; replay used the first ("),
        "{warning}"
    );
    assert!(report.steps[0].warning.is_none() && report.steps[2].warning.is_none());
    assert!(
        report.summary().contains("\nwarning: step 2 — "),
        "{}",
        report.summary()
    );
}
