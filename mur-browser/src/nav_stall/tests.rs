use super::*;
use crate::proxy::run_io;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, duplex};

const TARGET: &str = "http://127.0.0.1:50456/broken";

/// Verbatim shape of `@playwright/mcp` 0.0.82's navigate timeout.
fn timeout_error() -> Value {
    json!({"jsonrpc":"2.0","id":1,"result":{"isError":true,"content":[{"type":"text",
        "text":format!("### Error\nTimeoutError: browserBackend.callTool: Timeout 20000ms exceeded.\n\
            Call log:\n  - navigating to \"{TARGET}\", waiting until \"domcontentloaded\"\n")}]}})
}

fn tabs_at(url: &str) -> Value {
    json!({"jsonrpc":"2.0","id":"mur-1-1","result":{"content":[{"type":"text",
        "text":format!("### Result\n- 0: (current) []({url})")}]}})
}

#[test]
fn detects_only_navigation_timeouts() {
    assert!(is_navigation_timeout(&timeout_error()));
    let other = json!({"result":{"isError":true,"content":[{"type":"text",
        "text":"net::ERR_NAME_NOT_RESOLVED at http://x/"}]}});
    assert!(!is_navigation_timeout(&other));
    let action = json!({"result":{"isError":true,"content":[{"type":"text",
        "text":"Timeout 5000ms exceeded. waiting for locator"}]}});
    assert!(
        !is_navigation_timeout(&action),
        "action timeouts are not navigations"
    );
    let ok = json!({"result":{"content":[{"type":"text",
        "text":"Timeout 1ms exceeded navigating to "}]}});
    assert!(!is_navigation_timeout(&ok), "must be a tool error");
}

#[test]
fn reads_the_current_tab_url() {
    let tabs = json!({"result":{"content":[{"type":"text","text":
        "- 0: [Old](https://a.example/x)\n- 1: (current) [T (1)](https://b.example/y?q=1)"}]}});
    assert_eq!(current_tab_url(&tabs), Some("https://b.example/y?q=1"));
    let none = json!({"result":{"content":[{"type":"text","text":"No open tabs."}]}});
    assert_eq!(current_tab_url(&none), None);
    let failed = json!({"result":{"isError":true,"content":[{"type":"text",
        "text":"- 0: (current) [](https://b.example/)"}]}});
    assert_eq!(current_tab_url(&failed), None);
}

#[test]
fn same_origin_tab_gets_the_stalled_note_and_stays_an_error() {
    let r = annotated(TARGET, &timeout_error(), &tabs_at(TARGET)).unwrap();
    assert_eq!(r["result"]["isError"], true);
    assert_eq!(r["result"]["content"][0]["text"], STALLED_NOTE);
    assert!(
        r["result"]["content"][1]["text"]
            .as_str()
            .unwrap()
            .contains("Timeout 20000ms")
    );
}

#[test]
fn other_page_gets_the_not_committed_note() {
    let r = annotated(TARGET, &timeout_error(), &tabs_at("about:blank")).unwrap();
    assert_eq!(r["result"]["content"][0]["text"], NOT_COMMITTED_NOTE);
    let unknown = json!({"result":{"content":[{"type":"text","text":"No open tabs."}]}});
    assert!(annotated(TARGET, &timeout_error(), &unknown).is_none());
}

/// Server that times out navigate and answers `browser_tabs` with `tab_url`.
/// Proves the internal call does not deadlock the proxy.
async fn stall_server(
    inp: impl AsyncRead + Unpin,
    mut out: impl AsyncWrite + Unpin,
    tab_url: &'static str,
) {
    let mut lines = BufReader::new(inp).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let v: Value = serde_json::from_str(&line).unwrap();
        let mut resp = match v["params"]["name"].as_str() {
            Some("browser_navigate") => timeout_error(),
            Some("browser_tabs") => {
                assert_eq!(v["params"]["arguments"]["action"], "list");
                tabs_at(tab_url)
            }
            _ => json!({"jsonrpc":"2.0","result":{}}),
        };
        resp["id"] = v["id"].clone();
        out.write_all(format!("{resp}\n").as_bytes()).await.unwrap();
    }
}

async fn navigate_through_proxy(tab_url: &'static str) -> Value {
    let (mut agent_w, agent_in) = duplex(64 * 1024);
    let (agent_out, agent_r) = duplex(64 * 1024);
    let (server_in, srv_r) = duplex(64 * 1024);
    let (srv_w, server_out) = duplex(64 * 1024);
    tokio::spawn(stall_server(srv_r, srv_w, tab_url));
    let hook = NavStallHook::new(crate::proxy::ForwardHook);
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
        .expect("proxy deadlocked on the internal browser_tabs call")
        .unwrap()
        .unwrap();
    serde_json::from_str(&line).unwrap()
}

#[tokio::test]
async fn proxy_explains_a_stalled_navigation() {
    let r = navigate_through_proxy(TARGET).await;
    assert_eq!(r["id"], 7);
    assert_eq!(r["result"]["isError"], true);
    assert_eq!(r["result"]["content"][0]["text"], STALLED_NOTE);
}

#[tokio::test]
async fn proxy_explains_a_navigation_that_never_committed() {
    let r = navigate_through_proxy("about:blank").await;
    assert_eq!(r["result"]["content"][0]["text"], NOT_COMMITTED_NOTE);
}
