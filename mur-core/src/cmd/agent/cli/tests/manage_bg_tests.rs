//! #1921: a slow manage op (the MCP install probe) must not hold the event
//! loop. `run_manage_bg` returns at once, the status bar shows the label, and
//! the outcome arrives later as `StreamMsg::ManageDone`.

use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use super::super::stream::STREAM_CHANNEL_CAP;
use super::super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_manage_op_returns_immediately_and_reports_later() {
    let mut app = App::test_fixture();
    let (tx, mut rx) = mpsc::channel(STREAM_CHANNEL_CAP);
    // The op blocks until the test releases it — a stand-in for a probe
    // waiting on a server's `initialize`.
    let (release, gate) = std_mpsc::channel::<()>();

    run_manage_bg(
        &mut app,
        "probing MCP server slow".into(),
        &tx,
        move |_agent| {
            gate.recv().unwrap();
            Ok(("added slow".into(), None))
        },
    );

    // Returned while the op is still blocked: the loop is free to repaint.
    let (label, _) = app.manage_busy.as_ref().expect("busy while the op runs");
    assert_eq!(label, "probing MCP server slow");

    // A second op is refused rather than queued behind the first.
    run_manage_bg(&mut app, "other".into(), &tx, |_| Ok(("x".into(), None)));
    assert!(app.messages.iter().any(|m| m.text.contains("still busy")));

    release.send(()).unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("ManageDone arrives")
        .unwrap();
    assert!(matches!(msg, StreamMsg::ManageDone(Ok(_))));
    handle_stream(&mut app, msg, &tx);

    assert!(app.manage_busy.is_none(), "busy cleared on completion");
    assert!(app.messages.iter().any(|m| m.text == "added slow"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_manage_op_clears_busy_and_shows_the_error() {
    let mut app = App::test_fixture();
    let (tx, mut rx) = mpsc::channel(STREAM_CHANNEL_CAP);
    run_manage_bg(&mut app, "probing".into(), &tx, |_| {
        anyhow::bail!("probe timed out")
    });
    let msg = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap();
    handle_stream(&mut app, msg, &tx);
    assert!(app.manage_busy.is_none());
    assert!(
        app.messages
            .iter()
            .any(|m| m.text.contains("probe timed out")),
        "{:?}",
        app.messages
            .iter()
            .map(|m| m.text.clone())
            .collect::<Vec<_>>()
    );
}
