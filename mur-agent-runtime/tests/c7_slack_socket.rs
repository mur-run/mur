//! Tests for SlackSocketConn: WSS URL fetch, backoff logic.

use mur_agent_runtime::bridge::slack::{SlackError, SlackSocketConn};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn backoff_doubles_up_to_cap() {
    let mut conn = SlackSocketConn::new("xapp-test".into());
    assert_eq!(conn.backoff, Duration::from_secs(1));
    conn.advance_backoff();
    assert_eq!(conn.backoff, Duration::from_secs(2));
    conn.advance_backoff();
    assert_eq!(conn.backoff, Duration::from_secs(4));
    for _ in 0..10 {
        conn.advance_backoff();
    }
    assert_eq!(conn.backoff, Duration::from_secs(60));
}

#[test]
fn reset_backoff_returns_to_one_second() {
    let mut conn = SlackSocketConn::new("xapp-test".into());
    conn.advance_backoff();
    conn.advance_backoff();
    conn.reset_backoff();
    assert_eq!(conn.backoff, Duration::from_secs(1));
}

#[tokio::test]
async fn open_wss_url_returns_auth_error_on_401() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf).await;
        let resp = b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n";
        stream.write_all(resp).await.unwrap();
    });

    let client = reqwest::Client::new();
    let conn =
        SlackSocketConn::new_with_base_url("xapp-test".into(), format!("http://127.0.0.1:{port}"));
    let err = conn.open_wss_url(&client).await.unwrap_err();
    assert!(matches!(err, SlackError::Auth(401)), "got: {err:?}");
}
