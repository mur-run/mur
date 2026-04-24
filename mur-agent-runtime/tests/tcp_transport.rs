use mur_agent_runtime::transport::tcp::{TcpConnector, TcpTransportConfig, spawn_tcp_listener};
use mur_common::identity::AgentIdentity;
use std::sync::Arc;
use tokio::sync::mpsc;

#[tokio::test]
async fn end_to_end_handshake_and_echo() {
    let server_id = Arc::new(AgentIdentity::generate());
    let client_id = Arc::new(AgentIdentity::generate());

    // Handler: echo back the JSON-RPC payload
    let handler = Arc::new(|payload: Vec<u8>| async move {
        Ok::<_, std::io::Error>(payload) // echo
    });

    let cfg = TcpTransportConfig {
        bind: "127.0.0.1:0".into(), // kernel picks free port
    };
    let (shutdown_tx, shutdown_rx) = mpsc::channel(1);
    let handle = spawn_tcp_listener(cfg, server_id.clone(), handler, shutdown_rx)
        .await
        .unwrap();
    let actual_addr = handle.local_addr();

    // Client connects
    let server_pub = x25519_dalek::PublicKey::from(&server_id.to_x25519_static_secret());
    let mut conn = TcpConnector::dial(
        &actual_addr.to_string(),
        client_id.clone(),
        server_pub.as_bytes(),
    )
    .await
    .unwrap();

    let payload = br#"{"jsonrpc":"2.0","id":42,"method":"ping"}"#.to_vec();
    conn.send(&payload).await.unwrap();
    let reply = conn.recv().await.unwrap();
    assert_eq!(reply, payload);

    drop(shutdown_tx);
    handle.await_shutdown().await;
}
