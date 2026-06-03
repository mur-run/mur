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
    let client_pub =
        *x25519_dalek::PublicKey::from(&client_id.to_x25519_static_secret()).as_bytes();
    let handle = spawn_tcp_listener(
        cfg,
        server_id.clone(),
        handler,
        Arc::new(vec![client_pub]),
        shutdown_rx,
    )
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

#[tokio::test]
async fn rejects_peer_not_in_allowlist() {
    // A peer that completes the Noise handshake but whose static key is not in
    // the trusted_peers allowlist must not get a working session (fail-closed).
    let server_id = Arc::new(AgentIdentity::generate());
    let client_id = Arc::new(AgentIdentity::generate());
    let handler = Arc::new(|p: Vec<u8>| async move { Ok::<_, std::io::Error>(p) });
    let (shutdown_tx, shutdown_rx) = mpsc::channel(1);
    // Allowlist holds some *other* key, not the client's.
    let other =
        *x25519_dalek::PublicKey::from(&AgentIdentity::generate().to_x25519_static_secret())
            .as_bytes();
    let handle = spawn_tcp_listener(
        TcpTransportConfig {
            bind: "127.0.0.1:0".into(),
        },
        server_id.clone(),
        handler,
        Arc::new(vec![other]),
        shutdown_rx,
    )
    .await
    .unwrap();

    let server_pub = x25519_dalek::PublicKey::from(&server_id.to_x25519_static_secret());
    let dial = TcpConnector::dial(
        &handle.local_addr().to_string(),
        client_id,
        server_pub.as_bytes(),
    )
    .await;
    // The server drops the connection right after msg-3, so either the dial or
    // the first request round-trip fails — never a successful exchange.
    let failed = match dial {
        Err(_) => true,
        Ok(mut conn) => {
            let payload = br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#.to_vec();
            conn.send(&payload).await.is_err() || conn.recv().await.is_err()
        }
    };
    assert!(
        failed,
        "non-allowlisted peer must not get a working session"
    );
    drop(shutdown_tx);
}

#[tokio::test]
async fn dialer_aborts_on_mitm_mismatch() {
    let real_server = Arc::new(AgentIdentity::generate());
    let fake_pub = [0u8; 32]; // wrong

    let handler = Arc::new(|p: Vec<u8>| async move { Ok::<_, std::io::Error>(p) });
    let (tx, rx) = mpsc::channel(1);
    let handle = spawn_tcp_listener(
        TcpTransportConfig {
            bind: "127.0.0.1:0".into(),
        },
        real_server,
        handler,
        Arc::new(vec![]), // handshake fails before the allowlist check
        rx,
    )
    .await
    .unwrap();

    // The test MUST fail during handshake because initiator encrypts its
    // own static key to an unrelated pubkey.
    let client_id = Arc::new(AgentIdentity::generate());
    let res = TcpConnector::dial(&handle.local_addr().to_string(), client_id, &fake_pub).await;
    assert!(res.is_err(), "MITM-style mismatch must fail handshake");
    drop(tx);
}

#[tokio::test]
async fn dial_with_fallback_picks_first_working_candidate() {
    let server_id = Arc::new(AgentIdentity::generate());
    let client_id = Arc::new(AgentIdentity::generate());
    let bogus = [0u8; 32];

    let handler = Arc::new(|p: Vec<u8>| async move { Ok::<_, std::io::Error>(p) });
    let (tx, rx) = mpsc::channel(1);
    let handle = spawn_tcp_listener(
        TcpTransportConfig {
            bind: "127.0.0.1:0".into(),
        },
        server_id.clone(),
        handler,
        Arc::new(vec![
            *x25519_dalek::PublicKey::from(&client_id.to_x25519_static_secret()).as_bytes(),
        ]),
        rx,
    )
    .await
    .unwrap();

    let server_pub = x25519_dalek::PublicKey::from(&server_id.to_x25519_static_secret());
    let candidates = [bogus, *server_pub.as_bytes()];

    let mut conn =
        TcpConnector::dial_with_fallback(&handle.local_addr().to_string(), client_id, &candidates)
            .await
            .expect("must succeed via fallback");

    let payload = br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#.to_vec();
    conn.send(&payload).await.unwrap();
    let echo = conn.recv().await.unwrap();
    assert_eq!(echo, payload);

    drop(tx);
    handle.await_shutdown().await;
}

#[tokio::test]
async fn dial_with_fallback_all_bad_returns_last_error() {
    let server_id = Arc::new(AgentIdentity::generate());
    let client_id = Arc::new(AgentIdentity::generate());

    let handler = Arc::new(|p: Vec<u8>| async move { Ok::<_, std::io::Error>(p) });
    let (tx, rx) = mpsc::channel(1);
    let handle = spawn_tcp_listener(
        TcpTransportConfig {
            bind: "127.0.0.1:0".into(),
        },
        server_id,
        handler,
        Arc::new(vec![]), // all candidates bogus — handshake fails first
        rx,
    )
    .await
    .unwrap();

    let candidates = [[0u8; 32], [1u8; 32], [2u8; 32]];
    let res =
        TcpConnector::dial_with_fallback(&handle.local_addr().to_string(), client_id, &candidates)
            .await;
    assert!(res.is_err(), "all-bad fallback must error");

    drop(tx);
}

#[tokio::test]
async fn dial_with_fallback_empty_candidates_errors() {
    let client_id = Arc::new(AgentIdentity::generate());
    let res = TcpConnector::dial_with_fallback("127.0.0.1:1", client_id, &[]).await;
    assert!(res.is_err(), "empty candidates must be a hard error");
}
