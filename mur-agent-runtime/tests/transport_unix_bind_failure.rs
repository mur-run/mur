#![cfg(unix)]
//! A unix socket that cannot be bound must say so. The supervisor spawns the
//! listener and does not wait on it, so before this the bind error was dropped
//! and the runtime still printed `ready` with no socket (#2422: after the
//! Linux seal a bind outside every write grant fails exactly like this).

use mur_agent_runtime::protocol::a2a_server::Dispatcher;
use mur_agent_runtime::transport::unix_socket::serve_unix_reporting;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::mpsc;

#[test]
fn a_failed_bind_is_logged_with_its_path() {
    let tmp = TempDir::new().unwrap();
    // The parent dir does not exist, so `bind` fails on every platform —
    // the same shape as a bind Landlock refuses.
    let path = tmp.path().join("missing").join("agent.sock");
    let (_tx, rx) = mpsc::channel(1);

    let ((), log) = mur_test_log::capture(tracing::Level::WARN, || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(serve_unix_reporting(
                Arc::new(Dispatcher::new()),
                path.clone(),
                rx,
                None,
            ));
    });

    assert!(
        log.contains("unix socket transport failed") && log.contains(path.to_str().unwrap()),
        "a failed bind must name the socket path in a warning, got: {log:?}"
    );
}
