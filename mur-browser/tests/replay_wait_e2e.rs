//! Browser-backed lock: an `assert_text` right after a `goto` waits for a
//! page that renders its content after `load` (any SPA), and still fails when
//! the text never shows.
//!
//! Opt-in: set `MUR_BROWSER_E2E=1`. Everything else skips, because this needs
//! `node` plus the pinned `@playwright/mcp` install and its Chromium (both from
//! `mur browser setup`).
//!
//! Inside a sandboxed host, pass Chromium `--no-sandbox` through
//! `MUR_BROWSER_CHROMIUM_ARGS`; replay forwards it to the launch untouched.

use mur_browser::recorder::{Action, Mode, Run, Step};
use mur_browser::replay::{DEFAULT_ASSERT_TIMEOUT, ReplayOptions, StepStatus, replay_live};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const GATE: &str = "MUR_BROWSER_E2E";

fn gated() -> bool {
    if std::env::var_os(GATE).is_some_and(|v| !v.is_empty() && v != "0") {
        return true;
    }
    eprintln!("skipped: set {GATE}=1 to run the replay wait e2e");
    false
}

/// How long after `load` the page writes its text. Long enough that
/// `browser_navigate` has returned, well inside the assert timeout.
const RENDER_DELAY_MS: u64 = 1500;

const LATE_TEXT: &str = "rendered late";

/// A page whose text arrives from a timer, the way a SPA route renders after
/// the document has loaded.
fn page() -> String {
    format!(
        "<!doctype html><title>t</title><p id=\"out\">loading</p>\
         <script>setTimeout(() => {{ document.getElementById('out').textContent = \
         '{LATE_TEXT}'; }}, {RENDER_DELAY_MS});</script>"
    )
}

/// Serve `body` on 127.0.0.1 (replay's guard only allows http(s) URLs).
async fn serve(body: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let body = body.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(body.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}/")
}

fn step(n: u32, action: Action, value: &str) -> Step {
    Step {
        step: n,
        intent: "e2e step".to_string(),
        intent_auto: false,
        action,
        value: Some(value.to_string()),
        locators: Vec::new(),
        healed: false,
        last_hit: 0,
        ref_at_record: None,
    }
}

fn goto_then_assert(url: &str, text: &str) -> Run {
    Run {
        name: "late-text-e2e".to_string(),
        mode: Mode::Test,
        profile: None,
        recorded_at: chrono::Utc::now(),
        description: None,
        tags: vec![],
        replayed_at: None,
        replay_count: 0,
        steps: vec![
            step(1, Action::Goto, url),
            step(2, Action::AssertText, text),
        ],
    }
}

#[tokio::test]
async fn assert_text_waits_for_text_rendered_after_load() {
    if !gated() {
        return;
    }
    let url = serve(page()).await;
    let run = goto_then_assert(&url, LATE_TEXT);
    let report = replay_live(&run, &[], ReplayOptions::default(), None, None)
        .await
        .expect("replay ran");

    assert_eq!((report.passed, report.failed), (2, 0), "{report:#?}");
    assert_eq!(report.verdict(), "green");
}

#[tokio::test]
async fn assert_text_still_fails_when_the_text_never_shows() {
    if !gated() {
        return;
    }
    let url = serve(page()).await;
    let run = goto_then_assert(&url, "never rendered");
    let started = std::time::Instant::now();
    let report = replay_live(&run, &[], ReplayOptions::default(), None, None)
        .await
        .expect("replay ran");

    // Waiting must not turn a wrong page green: the step fails, and only
    // after the full timeout was spent looking.
    assert_eq!(report.steps[1].status, StepStatus::Failed, "{report:#?}");
    assert_eq!(report.verdict(), "red");
    assert!(
        started.elapsed() >= DEFAULT_ASSERT_TIMEOUT,
        "gave up after {:?}",
        started.elapsed()
    );
    let message = report.steps[1].message.as_deref().unwrap_or_default();
    assert!(message.contains("Text not found"), "{message}");
}
