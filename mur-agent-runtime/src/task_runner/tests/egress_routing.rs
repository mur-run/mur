//! `egress/denied` routing (#1907 follow-up): a denial that names a turn is
//! that turn's business and goes only to the connection watching it — the
//! same routing `step/*` frames use. Before this every denial rode the
//! socket broadcast, so a second murmur on the same agent printed the other
//! session's "was blocked" line.

use super::*;

fn denial(task_id: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": mur_common::egress_event::DENIED_METHOD,
        "params": {
            "agent": "mur", "server": "browser", "host": "img.x.com",
            "port": 443, "mode": "restricted", "reason": "not_allowed",
            "at": "2026-10-15T00:00:00Z", "task_id": task_id,
        },
    })
}

#[tokio::test]
async fn a_denial_naming_a_turn_reaches_only_that_turns_connection() {
    let (bcast, mut bcast_rx) = tokio::sync::mpsc::channel(4);
    let r = TaskRunner::new_stub_echo().with_notifier(bcast);
    let (a, mut a_rx) = tokio::sync::mpsc::channel(4);
    let (b, mut b_rx) = tokio::sync::mpsc::channel(4);
    r.register_client_notifier("task-a", a, true).await;
    r.register_client_notifier("task-b", b, true).await;

    r.route_egress_denied(denial(Some("task-a"))).await;

    assert_eq!(a_rx.try_recv().unwrap()["params"]["task_id"], "task-a");
    assert!(
        b_rx.try_recv().is_err(),
        "the other session must not see it"
    );
    assert!(
        bcast_rx.try_recv().is_err(),
        "nor may it ride the broadcast"
    );
}

/// Unattributed (no turn, or two turns sharing the server) and attributed to
/// a turn whose connection already went away: both fall back to the
/// broadcast, which is what every denial did before — never dropped.
#[tokio::test]
async fn an_unroutable_denial_falls_back_to_the_broadcast() {
    let (bcast, mut bcast_rx) = tokio::sync::mpsc::channel(4);
    let r = TaskRunner::new_stub_echo().with_notifier(bcast);
    let (a, mut a_rx) = tokio::sync::mpsc::channel(4);
    r.register_client_notifier("task-a", a, true).await;

    r.route_egress_denied(denial(None)).await;
    r.route_egress_denied(denial(Some("task-gone"))).await;

    assert!(bcast_rx.try_recv().is_ok());
    assert!(bcast_rx.try_recv().is_ok());
    assert!(a_rx.try_recv().is_err());
}
