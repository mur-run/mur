//! `McpPool` follows the hot-reloaded entry: an unchanged entry reuses the warm
//! client, an edited one starts a new client generation on the next call.

use mur_agent_runtime::mcp::pool::McpPool;
use mur_agent_runtime::sandbox::SandboxPolicy;
use mur_common::agent::McpServerEntry;
use std::sync::{Arc, Mutex};

fn mock(args: &[&str]) -> McpServerEntry {
    McpServerEntry {
        name: "mock".into(),
        command: env!("CARGO_BIN_EXE_mock_mcp").into(),
        args: args.iter().map(|a| a.to_string()).collect(),
        ..Default::default()
    }
}

#[tokio::test]
async fn an_edited_entry_starts_a_new_client_generation() {
    let pool = McpPool::new(vec![mock(&[])], SandboxPolicy::default(), None);
    let served = Arc::new(Mutex::new(mock(&[])));
    let source = served.clone();
    pool.attach_live(Box::new(move |name| {
        let e = source.lock().unwrap().clone();
        (e.name == name).then_some(e)
    }));

    let first = pool.client("mock").await.expect("first client");
    let again = pool.client("mock").await.expect("same entry");
    assert!(
        Arc::ptr_eq(&first, &again),
        "unchanged entry reuses the client"
    );

    *served.lock().unwrap() = mock(&["--edited"]);
    let next = pool.client("mock").await.expect("new generation");
    assert!(
        !Arc::ptr_eq(&first, &next),
        "edited entry gets a new client"
    );

    pool.shutdown().await;
}

#[tokio::test]
async fn a_server_the_live_profile_does_not_know_falls_back_to_boot() {
    let pool = McpPool::new(vec![mock(&[])], SandboxPolicy::default(), None);
    pool.attach_live(Box::new(|_| None));
    pool.client("mock").await.expect("boot entry still serves");
    pool.shutdown().await;
}
