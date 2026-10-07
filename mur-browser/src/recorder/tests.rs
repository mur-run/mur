use super::*;

fn step(action: Action, locators: &[&str]) -> Step {
    Step {
        step: 1,
        intent: "在搜尋框輸入 AirPods Pro".into(),
        intent_auto: false,
        action,
        value: Some("AirPods Pro".into()),
        locators: locators.iter().map(|s| s.to_string()).collect(),
        healed: false,
        last_hit: 0,
        ref_at_record: Some("@e21".into()),
    }
}

/// All 9 `Action` variants, for tests that must cover every one.
const ALL_ACTIONS: [Action; 9] = [
    Action::Goto,
    Action::Click,
    Action::Fill,
    Action::Select,
    Action::Press,
    Action::Hover,
    Action::AssertVisible,
    Action::AssertText,
    Action::AssertValue,
];

fn tools_call(name: &str, args: Value) -> Request {
    serde_json::from_value(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": name, "arguments": args},
    }))
    .unwrap()
}

/// 0.1 — `tool_name()` must round-trip through `action_for`'s lookup: for
/// every `Action`, wrapping its `tool_name()` in a `tools/call` request
/// and feeding it back through `action_for` must yield the same
/// `Action`. Where several tool names map to one `Action` (`Fill`),
/// `tool_name()` is only required to hit the one `action_for` picks
/// first — it does not need to be exhaustive over the whole fan-in.
#[test]
fn tool_name_round_trips_through_action_for() {
    for action in ALL_ACTIONS {
        let req = tools_call(action.tool_name(), serde_json::json!({}));
        assert_eq!(
            RecordHook::action_for(&req),
            Some(action),
            "tool_name({action:?}) = {:?} did not round-trip back through action_for",
            action.tool_name(),
        );
    }
}

/// 0.2 — `call_for`'s `arguments` must carry the step's value under the
/// exact key `value_for` reads it back out of, for every action that has
/// one (`Goto→url`, `Fill→text`, `Press→key`, asserts→text).
#[test]
fn call_for_uses_value_for_key_names() {
    let cases = [
        (Action::Goto, "url"),
        (Action::Fill, "text"),
        (Action::Press, "key"),
        (Action::AssertText, "text"),
        (Action::AssertValue, "value"),
    ];
    for (action, key) in cases {
        let s = step(action, &["role:button"]);
        let (_, args) = call_for(&s).unwrap();
        assert_eq!(
            args.get(key).and_then(Value::as_str),
            s.value.as_deref(),
            "call_for({action:?}) missing/mismatched {key:?} key: {args}"
        );
    }
}

/// 0.3 — locator-needing steps must carry a locator (`ref` or `element`)
/// in `call_for`'s arguments; `Goto` must carry neither.
#[test]
fn call_for_carries_locator_only_when_needed() {
    for action in ALL_ACTIONS {
        let locators: &[&str] = if action.needs_locator() {
            &["role:button[name=\"Go\"]"]
        } else {
            &[]
        };
        let s = step(action, locators);
        let (_, args) = call_for(&s).unwrap();
        let has_locator = args.get("ref").is_some() || args.get("element").is_some();
        assert_eq!(
            has_locator,
            action.needs_locator(),
            "call_for({action:?}) locator presence {has_locator} != needs_locator() {}: {args}",
            action.needs_locator(),
        );
    }
}

#[test]
fn assert_visible_rejects_without_role_locator() {
    let s = step(Action::AssertVisible, &["testid:checkout", "text:Checkout"]);
    assert_eq!(validate(s, false).unwrap_err(), Reject::NoRoleLocator);
}

#[test]
fn assert_visible_keeps_role_and_fallbacks() {
    let s = step(
        Action::AssertVisible,
        &["role:button[name=\"Checkout\"]", "testid:checkout"],
    );
    let s = validate(s, false).unwrap();
    assert_eq!(
        s.locators,
        vec!["role:button[name=\"Checkout\"]", "testid:checkout"]
    );
}

#[test]
fn other_actions_still_accept_testid_only() {
    for action in [Action::Click, Action::Hover, Action::AssertValue] {
        let s = step(action, &["testid:quantity-input"]);
        assert!(validate(s, false).is_ok(), "{action:?}");
    }
}

/// 0.0.82 `browser_verify_element_visible` carries `{ role, accessibleName }`
/// and no `ref`, so the role locator has to come from those arguments.
#[test]
fn locator_for_verify_visible_uses_role_and_accessible_name() {
    let hook = RecordHook::new(Run {
        name: "assert".into(),
        mode: Mode::Test,
        profile: None,
        recorded_at: chrono::Utc::now(),
        description: None,
        tags: vec![],
        replayed_at: None,
        replay_count: 0,
        steps: vec![],
    });
    let req = tools_call(
        "browser_verify_element_visible",
        serde_json::json!({"role": "button", "accessibleName": "Checkout"}),
    );
    assert_eq!(
        hook.locator_for(&req),
        vec!["role:button[name=\"Checkout\"]"]
    );
    let unnamed = tools_call(
        "browser_verify_element_visible",
        serde_json::json!({"role": "banner", "accessibleName": ""}),
    );
    assert_eq!(hook.locator_for(&unnamed), vec!["role:banner"]);
}

#[test]
fn locator_for_verify_visible_keeps_snapshot_fallbacks() {
    let mut hook = RecordHook::new(Run {
        name: "assert".into(),
        mode: Mode::Test,
        profile: None,
        recorded_at: chrono::Utc::now(),
        description: None,
        tags: vec![],
        replayed_at: None,
        replay_count: 0,
        steps: vec![],
    });
    hook.snapshot =
        crate::locator::parse_snapshot("- button \"Checkout\" [ref=e9] [data-testid=checkout]");
    let req = tools_call(
        "browser_verify_element_visible",
        serde_json::json!({"role": "button", "accessibleName": "Checkout"}),
    );
    let locators = hook.locator_for(&req);
    assert_eq!(locators[0], "role:button[name=\"Checkout\"]");
    assert!(
        locators.contains(&"testid:checkout".to_string()),
        "{locators:?}"
    );
}

#[test]
fn locator_for_accepts_target_as_ref() {
    let mut hook = RecordHook::new(Run {
        name: "target".into(),
        mode: Mode::Automation,
        profile: None,
        recorded_at: chrono::Utc::now(),
        description: None,
        tags: vec![],
        replayed_at: None,
        replay_count: 0,
        steps: vec![],
    });
    hook.snapshot = crate::locator::parse_snapshot("- button \"Add to cart\" [ref=e5]");
    let req = tools_call(
        "browser_click",
        serde_json::json!({"element": "Add to cart", "target": "e5"}),
    );
    assert_eq!(
        hook.locator_for(&req)[0],
        "role:button[name=\"Add to cart\"]"
    );
}
#[test]
fn value_for_select_keeps_first_array_entry() {
    let args = serde_json::json!({"target": "e4", "values": ["M"]});
    assert_eq!(
        RecordHook::value_for(Action::Select, Some(&args)),
        Some("M".to_string())
    );
}
#[test]
fn reject_when_no_locator() {
    let s = step(Action::Click, &[]);
    assert_eq!(validate(s, false).unwrap_err(), Reject::NoLocator);
}

#[test]
fn reject_when_only_unstable_locators() {
    // the `agent-browser去pchome` yaml:59 case — nothing but an @ref-ish css chain
    let s = step(Action::Click, &["css:div > div > ul > li:nth-child(3) > a"]);
    assert_eq!(validate(s, false).unwrap_err(), Reject::NoLocator);
}

#[test]
fn prunes_unstable_keeps_stable() {
    let s = step(
        Action::Click,
        &[
            "css:.css-1x2y3z",
            "role:button[name=\"登入\"]",
            "testid:login",
        ],
    );
    let s = validate(s, false).unwrap();
    assert_eq!(
        s.locators,
        vec!["role:button[name=\"登入\"]", "testid:login"]
    );
}

#[test]
fn reject_short_intent() {
    let mut s = step(Action::Click, &["testid:x"]);
    s.intent = "點".into();
    assert_eq!(validate(s, false).unwrap_err(), Reject::IntentTooShort);
}

#[test]
fn reject_raw_secret_in_password_field() {
    let mut s = step(Action::Fill, &["role:textbox[name=\"密碼\"]"]);
    s.value = Some("Hunter2Hunter2x9".into());
    assert_eq!(validate(s.clone(), true).unwrap_err(), Reject::RawSecret);
    // same value in a non-password field is fine
    assert!(validate(s.clone(), false).is_ok());
    // placeholder is fine even in a password field
    s.value = Some("{{secret:pchome/PASSWORD}}".into());
    assert!(validate(s, true).is_ok());
}

#[test]
fn goto_needs_no_locator() {
    let mut s = step(Action::Goto, &[]);
    s.value = Some("https://24h.pchome.com.tw".into());
    assert!(validate(s, false).is_ok());
}

#[test]
fn yaml_round_trip_matches_spec_shape() {
    let run = Run {
        name: "smoke".into(),
        mode: Mode::Test,
        profile: Some("pchome".into()),
        recorded_at: chrono::DateTime::parse_from_rfc3339("2026-09-10T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
        description: None,
        tags: vec![],
        replayed_at: None,
        replay_count: 0,
        steps: vec![step(
            Action::Fill,
            &["role:searchbox[name=\"搜尋\"]", "testid:search-input"],
        )],
    };
    let y = to_yaml(&run).unwrap();
    assert!(y.contains("intent: 在搜尋框輸入 AirPods Pro"), "{y}");
    assert!(y.contains("action: fill"), "{y}");
    assert!(y.contains("- role:searchbox[name=\"搜尋\"]"), "{y}");
    assert!(!y.contains("healed"), "false flags are omitted: {y}");
    assert_eq!(from_yaml(&y).unwrap(), run);
}

#[test]
fn schema_exports() {
    let s = json_schema();
    assert!(s["title"].as_str().is_some() || s["$schema"].as_str().is_some());
}

#[test]
fn record_hook_persists_successful_navigation_as_first_step() {
    // Public seam for slice 2: the proxy hook, not recorder internals.
    // This deliberately fails until `RecordHook` exists and owns the run.
    let hook = RecordHook::new(Run {
        name: "smoke".into(),
        mode: Mode::Test,
        profile: None,
        recorded_at: chrono::Utc::now(),
        description: None,
        tags: vec![],
        replayed_at: None,
        replay_count: 0,
        steps: vec![],
    });
    assert!(hook.run().steps.is_empty());
}

#[tokio::test]
async fn proxy_does_not_record_mcp_tool_failures() {
    // Exercise the public proxy seam: MCP reports tool failures inside a
    // JSON-RPC `result` with `isError`, rather than a JSON-RPC error.
    use crate::proxy::run_io;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, duplex};

    async fn server(mut input: impl AsyncRead + Unpin, mut output: impl AsyncWrite + Unpin) {
        let mut lines = BufReader::new(&mut input).lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            let request: Value = serde_json::from_str(&line).unwrap();
            let response = json!({
                "jsonrpc": "2.0",
                "id": request["id"].clone(),
                "result": {
                    "isError": true,
                    "content": [{"type": "text", "text": "button not found"}]
                }
            });
            output
                .write_all(response.to_string().as_bytes())
                .await
                .unwrap();
            output.write_all(b"\n").await.unwrap();
            output.flush().await.unwrap();
        }
    }

    let dir =
        std::env::temp_dir().join(format!("mur-browser-failed-action-{}", std::process::id()));
    let actions = dir.join("actions.yaml");
    let hook = RecordHook::with_actions_path(
        Run {
            name: "failed-click".into(),
            mode: Mode::Test,
            profile: None,
            recorded_at: chrono::Utc::now(),
            description: None,
            tags: vec![],
            replayed_at: None,
            replay_count: 0,
            steps: vec![],
        },
        actions.clone(),
    );
    let (mut agent_write, agent_input) = duplex(16 * 1024);
    let (agent_output, mut agent_read) = duplex(16 * 1024);
    let (server_input, server_read) = duplex(16 * 1024);
    let (server_write, server_output) = duplex(16 * 1024);
    tokio::spawn(server(server_read, server_write));
    tokio::spawn(run_io(
        agent_input,
        agent_output,
        server_input,
        server_output,
        hook,
    ));

    agent_write.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"browser_click\",\"arguments\":{\"element\":\"missing button\"}}}\n").await.unwrap();
    let mut lines = BufReader::new(&mut agent_read).lines();
    let response: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(response["result"]["isError"], true);
    assert!(!actions.exists(), "failed MCP calls must not be recorded");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn proxy_records_intent_and_successful_navigation_to_yaml() {
    use crate::proxy::run_io;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, duplex};

    async fn server(mut input: impl AsyncRead + Unpin, mut output: impl AsyncWrite + Unpin) {
        let mut lines = BufReader::new(&mut input).lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            let request: Value = serde_json::from_str(&line).unwrap();
            let response = json!({
                "jsonrpc": "2.0",
                "id": request["id"].clone(),
                "result": {"content": [{"type": "text", "text": "ok"}]}
            });
            output
                .write_all(response.to_string().as_bytes())
                .await
                .unwrap();
            output.write_all(b"\n").await.unwrap();
            output.flush().await.unwrap();
        }
    }

    let suffix = format!("mur-browser-record-{}", std::process::id());
    let dir = std::env::temp_dir().join(suffix);
    let actions = dir.join("actions.yaml");
    let hook = RecordHook::with_actions_path(
        Run {
            name: "smoke".into(),
            mode: Mode::Test,
            profile: None,
            recorded_at: chrono::Utc::now(),
            description: None,
            tags: vec![],
            replayed_at: None,
            replay_count: 0,
            steps: vec![],
        },
        actions.clone(),
    );
    let (mut agent_write, agent_input) = duplex(16 * 1024);
    let (agent_output, mut agent_read) = duplex(16 * 1024);
    let (server_input, server_read) = duplex(16 * 1024);
    let (server_write, server_output) = duplex(16 * 1024);
    tokio::spawn(server(server_read, server_write));
    tokio::spawn(run_io(
        agent_input,
        agent_output,
        server_input,
        server_output,
        hook,
    ));

    agent_write.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"mur_intent\",\"arguments\":{\"text\":\"Open the MUR homepage\"}}}\n").await.unwrap();
    let mut lines = BufReader::new(&mut agent_read).lines();
    let intent_reply: Value =
        serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(intent_reply["result"]["queued"], true);

    agent_write.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"browser_navigate\",\"arguments\":{\"url\":\"https://example.test\"}}}\n").await.unwrap();
    let navigation_reply: Value =
        serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
    assert_eq!(navigation_reply["id"], 2);

    let saved = from_yaml(&std::fs::read_to_string(&actions).unwrap()).unwrap();
    assert_eq!(saved.steps.len(), 1);
    assert_eq!(saved.steps[0].intent, "Open the MUR homepage");
    assert!(!saved.steps[0].intent_auto);
    assert_eq!(saved.steps[0].action, Action::Goto);
    assert_eq!(
        saved.steps[0].value.as_deref(),
        Some("https://example.test")
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn proxy_snapshot_then_click_records_stable_locator_candidates() {
    use crate::proxy::run_io;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, duplex};

    async fn server(mut input: impl AsyncRead + Unpin, mut output: impl AsyncWrite + Unpin) {
        let mut lines = BufReader::new(&mut input).lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            let request: Value = serde_json::from_str(&line).unwrap();
            let text = match request["params"]["name"].as_str() {
                Some("browser_snapshot") => {
                    "- button \"Submit\" [ref=e4] [data-testid=submit-button]"
                }
                _ => "ok",
            };
            let response = json!({
                "jsonrpc": "2.0",
                "id": request["id"].clone(),
                "result": {"content": [{"type": "text", "text": text}]}
            });
            output
                .write_all(response.to_string().as_bytes())
                .await
                .unwrap();
            output.write_all(b"\n").await.unwrap();
            output.flush().await.unwrap();
        }
    }

    let dir =
        std::env::temp_dir().join(format!("mur-browser-snapshot-click-{}", std::process::id()));
    let actions = dir.join("actions.yaml");
    let hook = RecordHook::with_actions_path(
        Run {
            name: "snapshot-click".into(),
            mode: Mode::Test,
            profile: None,
            recorded_at: chrono::Utc::now(),
            description: None,
            tags: vec![],
            replayed_at: None,
            replay_count: 0,
            steps: vec![],
        },
        actions.clone(),
    );
    let (mut agent_write, agent_input) = duplex(16 * 1024);
    let (agent_output, mut agent_read) = duplex(16 * 1024);
    let (server_input, server_read) = duplex(16 * 1024);
    let (server_write, server_output) = duplex(16 * 1024);
    tokio::spawn(server(server_read, server_write));
    tokio::spawn(run_io(
        agent_input,
        agent_output,
        server_input,
        server_output,
        hook,
    ));
    let mut lines = BufReader::new(&mut agent_read).lines();

    for request in [
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_snapshot","arguments":{}}}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"browser_click","arguments":{"ref":"@e4"}}}),
    ] {
        agent_write
            .write_all(request.to_string().as_bytes())
            .await
            .unwrap();
        agent_write.write_all(b"\n").await.unwrap();
        let reply: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert!(reply.get("error").is_none(), "{reply}");
    }

    let saved = from_yaml(&std::fs::read_to_string(&actions).unwrap()).unwrap();
    assert_eq!(saved.steps.len(), 1);
    assert_eq!(saved.steps[0].action, Action::Click);
    assert_eq!(saved.steps[0].ref_at_record.as_deref(), Some("@e4"));
    assert_eq!(
        saved.steps[0].locators,
        vec![
            "role:button[name=\"Submit\"]".to_string(),
            "testid:submit-button".to_string(),
            "text:Submit".to_string(),
        ]
    );
    let _ = std::fs::remove_dir_all(dir);
}

const LEGACY_RUN_YAML: &str = "\
name: legacy
mode: test
recorded_at: 2026-10-01T08:00:00Z
steps: []
";

#[test]
fn legacy_yaml_without_new_fields_round_trips_byte_identical() {
    let run = from_yaml(LEGACY_RUN_YAML).unwrap();
    assert_eq!(to_yaml(&run).unwrap(), LEGACY_RUN_YAML);
}

#[test]
fn new_fields_parse_and_default() {
    let bare = from_yaml(LEGACY_RUN_YAML).unwrap();
    assert_eq!(bare.description, None);
    assert!(bare.tags.is_empty());
    assert_eq!(bare.replayed_at, None);
    assert_eq!(bare.replay_count, 0);

    let full = from_yaml(
        "\
name: full
mode: automation
recorded_at: 2026-10-01T08:00:00Z
description: 每日登入
tags:
- daily
- shop
replayed_at: 2026-10-05T09:30:00Z
replay_count: 7
steps: []
",
    )
    .unwrap();
    assert_eq!(full.description.as_deref(), Some("每日登入"));
    assert_eq!(full.tags, ["daily", "shop"]);
    assert_eq!(
        full.replayed_at.unwrap().to_rfc3339(),
        "2026-10-05T09:30:00+00:00"
    );
    assert_eq!(full.replay_count, 7);
    // And it survives a round trip.
    assert_eq!(from_yaml(&to_yaml(&full).unwrap()).unwrap(), full);
}
