use super::*;
use crate::tools::ToolExecutor;

fn tool(home: &std::path::Path) -> BrowserRequestTool {
    BrowserRequestTool {
        agent_home: home.to_path_buf(),
    }
}

fn pending(home: &std::path::Path) -> Vec<mur_browser_request::PendingRequest> {
    mur_browser_request::read_pending(home, chrono::Utc::now())
}

#[tokio::test]
async fn a_request_is_recorded_as_an_origin_and_the_reply_says_stop() {
    let home = tempfile::tempdir().unwrap();
    let out = tool(home.path())
        .execute(serde_json::json!({
            "site": "http://gitea.twdd.tw:3000/user/login",
            "reason": "review PR 12",
            "login": true
        }))
        .await
        .unwrap();
    let p = pending(home.path());
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].site.origin(), "http://gitea.twdd.tw:3000");
    assert!(p[0].login);
    assert!(
        out.text.contains("http://gitea.twdd.tw:3000"),
        "{}",
        out.text
    );
    assert!(out.text.contains("Stop now"), "{}", out.text);
    assert!(!out.text.contains("replaces"), "{}", out.text);
}

#[tokio::test]
async fn a_repeat_for_the_same_origin_replaces_and_says_so() {
    let home = tempfile::tempdir().unwrap();
    let t = tool(home.path());
    let call = |login: bool| serde_json::json!({"site": "example.com", "reason": "read docs", "login": login});
    t.execute(call(true)).await.unwrap();
    let out = t.execute(call(false)).await.unwrap();
    assert!(out.text.contains("replaces"), "{}", out.text);
    let p = pending(home.path());
    assert_eq!(p.len(), 1);
    assert!(!p[0].login);
}

#[tokio::test]
async fn bad_input_is_refused_and_nothing_is_written() {
    let home = tempfile::tempdir().unwrap();
    let t = tool(home.path());
    for input in [
        serde_json::json!({"reason": "r", "login": true}),
        serde_json::json!({"site": "example.com", "login": true}),
        serde_json::json!({"site": "example.com", "reason": "r"}),
        serde_json::json!({"site": "example.com", "reason": "r", "login": "yes"}),
        serde_json::json!({"site": "file:///etc/passwd", "reason": "r", "login": false}),
        serde_json::json!({"site": "https://u:p@example.com", "reason": "r", "login": true}),
    ] {
        let e = t.execute(input.clone()).await.unwrap_err();
        assert!(matches!(e, ToolError::InvalidInput(_)), "{input}: {e}");
    }
    assert!(pending(home.path()).is_empty());
}

#[test]
fn the_tool_is_tiered_inside_the_auto_ceiling() {
    // #2360 live check: the `browser_` prefix put it at NetworkEgress, so it
    // asked on every call and a one-shot `mur agent send` could never use it.
    let tier = mur_common::hitl::tool_tier::classify(BROWSER_REQUEST, None);
    assert!(mur_common::hitl::tier_may_be_granted(tier), "{tier:?}");
    assert_eq!(
        crate::task_runner::call_floor(&[], BROWSER_REQUEST, None),
        None
    );
}

#[test]
fn the_schema_never_offers_a_profile() {
    let def = tool(std::path::Path::new("/nonexistent")).def();
    let props = def.input_schema["properties"].as_object().unwrap();
    let mut keys: Vec<&str> = props.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["login", "reason", "site"]);
    assert_eq!(
        def.input_schema["required"],
        serde_json::json!(["site", "reason", "login"])
    );
}

#[test]
fn prompt_section_names_the_route_and_forbids_asking_for_commands() {
    let s = prompt_section();
    assert!(s.starts_with("\n\n## Browser\n"), "{s:?}");
    assert!(s.contains("call `browser_request`"));
    assert!(s.contains("Never name a profile"));
    assert!(s.contains("Do not ask the user to run commands"));
}
