use super::*;
use crate::proxy::run_io;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, duplex};

const TARGET: &str = "https://www.example.com/a";

fn race_error() -> Value {
    json!({"jsonrpc":"2.0","id":1,"result":{"isError":true,"content":[{"type":"text",
        "text":format!("### Error\nError: page.goto: Navigation to \"{TARGET}\" {ERROR_PAGE_RACE}")}]}})
}

fn snapshot_at(url: &str) -> Value {
    json!({"jsonrpc":"2.0","id":"mur-1-1","result":{"content":[{"type":"text",
        "text":format!("### Page\n{PAGE_URL_PREFIX}{url}\n- Page Title: ok")}]}})
}

#[test]
fn detects_only_the_error_page_race() {
    assert!(is_error_page_race(&race_error()));
    let other = json!({"result":{"isError":true,"content":[{"type":"text",
        "text":"net::ERR_NAME_NOT_RESOLVED"}]}});
    assert!(!is_error_page_race(&other));
    let not_error = json!({"result":{"content":[{"type":"text","text":ERROR_PAGE_RACE}]}});
    assert!(!is_error_page_race(&not_error), "must be a tool error");
}

#[test]
fn same_origin_snapshot_confirms() {
    let r = confirmed_result(TARGET, &snapshot_at("https://www.example.com/final")).unwrap();
    assert_eq!(r["content"][0]["text"], CONFIRMED_NOTE);
    assert!(
        r["content"][1]["text"]
            .as_str()
            .unwrap()
            .contains("Page Title: ok")
    );
    assert!(r.get("isError").is_none());
}

#[test]
fn error_page_other_host_or_failed_snapshot_do_not_confirm() {
    assert!(confirmed_result(TARGET, &snapshot_at("chrome-error://chromewebdata/")).is_none());
    assert!(confirmed_result(TARGET, &snapshot_at("https://evil.example.net/")).is_none());
    assert!(confirmed_result(TARGET, &snapshot_at("http://www.example.com/a")).is_none());
    assert!(confirmed_result(TARGET, &snapshot_at("https://www.example.com:8443/a")).is_none());
    let failed = json!({"result":{"isError":true,"content":[{"type":"text",
        "text":format!("{PAGE_URL_PREFIX}{TARGET}")}]}});
    assert!(confirmed_result(TARGET, &failed).is_none());
}

/// Server that fails navigate with the race and answers snapshot with
/// `snap_url`. Proves the internal call does not deadlock the proxy.
async fn race_server(
    inp: impl AsyncRead + Unpin,
    mut out: impl AsyncWrite + Unpin,
    snap_url: &'static str,
) {
    let mut lines = BufReader::new(inp).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let v: Value = serde_json::from_str(&line).unwrap();
        let mut resp = match v["params"]["name"].as_str() {
            Some("browser_navigate") => race_error(),
            Some("browser_snapshot") => snapshot_at(snap_url),
            _ => json!({"jsonrpc":"2.0","result":{}}),
        };
        resp["id"] = v["id"].clone();
        out.write_all(format!("{resp}\n").as_bytes()).await.unwrap();
    }
}

async fn navigate_through_proxy(snap_url: &'static str) -> Value {
    let (mut agent_w, agent_in) = duplex(64 * 1024);
    let (agent_out, agent_r) = duplex(64 * 1024);
    let (server_in, srv_r) = duplex(64 * 1024);
    let (srv_w, server_out) = duplex(64 * 1024);
    tokio::spawn(race_server(srv_r, srv_w, snap_url));
    let hook = NavConfirmHook::new(crate::proxy::ForwardHook);
    tokio::spawn(run_io(agent_in, agent_out, server_in, server_out, hook));
    let req = json!({"jsonrpc":"2.0","id":7,"method":"tools/call",
        "params":{"name":"browser_navigate","arguments":{"url":TARGET}}});
    agent_w
        .write_all(format!("{req}\n").as_bytes())
        .await
        .unwrap();
    let mut lines = BufReader::new(agent_r).lines();
    let line = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .expect("proxy deadlocked on the internal snapshot call")
        .unwrap()
        .unwrap();
    serde_json::from_str(&line).unwrap()
}

#[tokio::test]
async fn proxy_replaces_race_error_when_page_loaded() {
    let r = navigate_through_proxy("https://www.example.com/a").await;
    assert_eq!(r["id"], 7);
    assert!(r["result"].get("isError").is_none(), "{r}");
    assert_eq!(r["result"]["content"][0]["text"], CONFIRMED_NOTE);
}

#[tokio::test]
async fn proxy_keeps_race_error_when_tab_is_on_error_page() {
    let r = navigate_through_proxy("chrome-error://chromewebdata/").await;
    assert_eq!(r["id"], 7);
    assert_eq!(r["result"]["isError"], true);
}
