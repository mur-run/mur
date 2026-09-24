use super::stdio::live_args;
use super::*;
use crate::recorder::from_yaml;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[test]
fn live_args_enable_testing_caps_for_assert_steps() {
    // browser_verify_* tools are opt-in via --caps=testing in @playwright/mcp.
    let args = live_args(None);
    assert!(args.contains(&"--caps=testing".to_owned()), "{args:?}");
    assert!(args.contains(&"--browser=chromium".to_owned()));
    let with_state = live_args(Some(std::path::Path::new("/tmp/s.json")));
    assert!(with_state.contains(&"--storage-state=/tmp/s.json".to_owned()));
}

fn run(yaml: &str) -> Run {
    from_yaml(yaml).unwrap()
}

const THREE: &str = r#"
name: demo
mode: test
recorded_at: 2026-09-23T00:00:00Z
steps:
- step: 1
  intent: 前往登入頁面
  action: goto
  value: https://app.example.com/login
- step: 2
  intent: 點擊登入按鈕
  action: click
  locators: ['testid:gone', 'role:button[name="Sign in"]']
- step: 3
  intent: 確認歡迎文字
  action: assert_text
  value: Welcome
  locators: ['text:Welcome']
"#;

const SNAPSHOT: &str = "- button \"Sign in\" [ref=e7]\n- heading \"Welcome\" [ref=e9]";

/// Scripted caller: records every call, answers from a queue.
#[derive(Default)]
struct Fake {
    calls: Vec<(String, Value)>,
    fail_tool: Option<&'static str>,
}

impl ToolCaller for Fake {
    async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        self.calls.push((name.to_owned(), arguments));
        let text = if name == "browser_snapshot" {
            SNAPSHOT
        } else {
            "ok"
        };
        let is_error = self.fail_tool == Some(name);
        Ok(json!({"content": [{"type": "text", "text": text}], "isError": is_error}))
    }
}

#[test]
fn dry_run_skips_every_step() {
    let report = dry_run(&run(THREE), &[]).unwrap();
    assert_eq!((report.total, report.passed, report.failed), (3, 0, 0));
    assert!(report.steps.iter().all(|s| s.status == StepStatus::Skipped));
    assert_eq!(report.verdict(), "green");
}

#[test]
fn off_allowlist_goto_is_rejected_naming_the_host() {
    let err = dry_run(&run(THREE), &["other.test".into()]).unwrap_err();
    let text = format!("{err:#}");
    assert!(text.contains("app.example.com"), "{text}");
    assert!(text.contains("step 1"), "{text}");
}

#[tokio::test]
async fn blocked_goto_makes_no_calls_at_all() {
    let mut fake = Fake::default();
    let err = replay_with(
        &run(THREE),
        &["other.test".into()],
        ReplayOptions::default(),
        &mut fake,
    )
    .await;
    assert!(err.is_err());
    assert!(fake.calls.is_empty(), "{:?}", fake.calls);
}

#[tokio::test]
async fn resolves_locators_in_order_and_sends_ref() {
    let mut fake = Fake::default();
    let report = replay_with(
        &run(THREE),
        &["example.com".into()],
        ReplayOptions::default(),
        &mut fake,
    )
    .await
    .unwrap();
    assert_eq!((report.passed, report.failed), (3, 0), "{report:?}");
    // testid:gone misses, role:… hits.
    assert_eq!(
        report.steps[1].locator_used.as_deref(),
        Some("role:button[name=\"Sign in\"]")
    );
    let click = fake
        .calls
        .iter()
        .find(|(n, _)| n == "browser_click")
        .unwrap();
    assert_eq!(click.1["target"], "e7");
    assert!(click.1.get("ref").is_none());
    let nav = &fake.calls[0];
    assert_eq!(nav.0, "browser_navigate");
    assert_eq!(nav.1["url"], "https://app.example.com/login");
}

#[tokio::test]
async fn test_mode_stops_at_first_failure() {
    let mut fake = Fake {
        fail_tool: Some("browser_click"),
        ..Fake::default()
    };
    let report = replay_with(&run(THREE), &[], ReplayOptions::default(), &mut fake)
        .await
        .unwrap();
    let statuses: Vec<_> = report.steps.iter().map(|s| s.status).collect();
    assert_eq!(
        statuses,
        [StepStatus::Passed, StepStatus::Failed, StepStatus::Skipped]
    );
    assert!(
        report.steps[1]
            .message
            .as_deref()
            .unwrap()
            .contains("點擊登入按鈕")
    );
    assert_eq!(report.verdict(), "red");
}

#[tokio::test]
async fn testid_missing_from_snapshot_falls_back_to_css_selector() {
    // Real Playwright snapshots never expose data-testid, so a
    // testid-only step must be sent as a selector for Playwright to find.
    let yaml = THREE.replace(
        "['testid:gone', 'role:button[name=\"Sign in\"]']",
        "['testid:product-thumbnail']",
    );
    let mut fake = Fake::default();
    let report = replay_with(&run(&yaml), &[], ReplayOptions::default(), &mut fake)
        .await
        .unwrap();
    assert_eq!(report.steps[1].status, StepStatus::Passed, "{report:?}");
    assert_eq!(
        report.steps[1].locator_used.as_deref(),
        Some("testid:product-thumbnail")
    );
    let click = fake
        .calls
        .iter()
        .find(|(n, _)| n == "browser_click")
        .unwrap();
    assert_eq!(click.1["target"], "[data-testid=\"product-thumbnail\"]");
}

#[tokio::test]
async fn snapshot_hit_beats_testid_selector_fallback() {
    // testid:gone is listed first but only the role hits the snapshot;
    // the verified ref wins over an unverified selector.
    let mut fake = Fake::default();
    let report = replay_with(&run(THREE), &[], ReplayOptions::default(), &mut fake)
        .await
        .unwrap();
    assert_eq!(
        report.steps[1].locator_used.as_deref(),
        Some("role:button[name=\"Sign in\"]")
    );
}

#[test]
fn testid_selector_escapes_quotes_and_backslashes() {
    assert_eq!(testid_selector(r#"a"b\c"#), r#"[data-testid="a\"b\\c"]"#);
}

#[tokio::test]
async fn locator_miss_fails_with_candidates_listed() {
    let yaml = THREE.replace(
        "['testid:gone', 'role:button[name=\"Sign in\"]']",
        "['text:gone', 'role:button[name=\"Log in\"]']",
    );
    let mut fake = Fake::default();
    let report = replay_with(&run(&yaml), &[], ReplayOptions::default(), &mut fake)
        .await
        .unwrap();
    assert_eq!(report.steps[1].status, StepStatus::Failed);
    let msg = report.steps[1].message.clone().unwrap();
    assert!(
        msg.contains("no locator matched") && msg.contains("text:gone"),
        "{msg}"
    );
}

#[tokio::test]
async fn automation_mode_does_not_fail_on_assert() {
    let yaml = THREE.replace("mode: test", "mode: automation");
    let mut fake = Fake {
        fail_tool: Some("browser_verify_text_visible"),
        ..Fake::default()
    };
    let report = replay_with(&run(&yaml), &[], ReplayOptions::default(), &mut fake)
        .await
        .unwrap();
    assert_eq!(report.failed, 0, "{report:?}");
    assert_eq!(report.steps[2].status, StepStatus::Skipped);
}

#[tokio::test]
async fn select_values_are_sent_as_array() {
    let yaml = r#"
name: s
mode: test
recorded_at: 2026-09-23T00:00:00Z
steps:
- step: 1
  intent: 選擇尺寸選項
  action: select
  value: M
  locators: ['role:button[name="Sign in"]']
"#;
    let mut fake = Fake::default();
    replay_with(&run(yaml), &[], ReplayOptions::default(), &mut fake)
        .await
        .unwrap();
    let select = fake
        .calls
        .iter()
        .find(|(n, _)| n == "browser_select_option")
        .unwrap();
    assert_eq!(select.1["values"], json!(["M"]));
}

/// Replay a one-step run and return the report plus the args of `tool`.
async fn one_step(
    action: &str,
    value: Option<&str>,
    locators: &str,
    tool: &str,
) -> (ReplayReport, Option<Value>) {
    let value = value
        .map(|v| format!("  value: \"{v}\"\n"))
        .unwrap_or_default();
    let yaml = format!(
        "name: s\nmode: test\nrecorded_at: 2026-09-23T00:00:00Z\nsteps:\n- step: 1\n  intent: 確認這一步\n  action: {action}\n{value}  locators: {locators}\n"
    );
    let mut fake = Fake::default();
    let report = replay_with(&run(&yaml), &[], ReplayOptions::default(), &mut fake)
        .await
        .unwrap();
    let args = fake
        .calls
        .into_iter()
        .find(|(n, _)| n == tool)
        .map(|(_, a)| a);
    (report, args)
}

#[tokio::test]
async fn assert_visible_sends_role_and_accessible_name() {
    // 0.0.82 schema: { role, accessibleName } — nothing else.
    let (report, args) = one_step(
        "assert_visible",
        None,
        "['role:button[name=\"Sign in\"]']",
        "browser_verify_element_visible",
    )
    .await;
    assert_eq!(report.passed, 1, "{report:?}");
    assert_eq!(
        args.unwrap(),
        json!({"role": "button", "accessibleName": "Sign in"})
    );
}

#[tokio::test]
async fn assert_visible_without_snapshot_node_fails_clearly() {
    // A testid selector carries no role/name, so verify_element_visible
    // cannot be called; say so instead of sending a malformed call.
    let (report, args) = one_step(
        "assert_visible",
        None,
        "['testid:product-thumbnail']",
        "browser_verify_element_visible",
    )
    .await;
    assert_eq!(report.steps[0].status, StepStatus::Failed);
    assert!(args.is_none());
    let msg = report.steps[0].message.clone().unwrap();
    assert!(msg.contains("role"), "{msg}");
}

#[tokio::test]
async fn assert_text_sends_only_text() {
    let (report, args) = one_step(
        "assert_text",
        Some("Welcome"),
        "['text:Welcome']",
        "browser_verify_text_visible",
    )
    .await;
    assert_eq!(report.passed, 1, "{report:?}");
    assert_eq!(args.unwrap(), json!({"text": "Welcome"}));
}

#[tokio::test]
async fn assert_value_sends_type_element_target_value() {
    // Live step 9: testid-only locator falls back to a selector, and
    // with no snapshot node the type defaults to textbox.
    let (report, args) = one_step(
        "assert_value",
        Some("2"),
        "['testid:quantity-input']",
        "browser_verify_value",
    )
    .await;
    assert_eq!(report.passed, 1, "{report:?}");
    assert_eq!(
        args.unwrap(),
        json!({
            "type": "textbox",
            "element": "確認這一步",
            "target": "[data-testid=\"quantity-input\"]",
            "value": "2",
        })
    );
}

#[test]
fn verify_value_type_maps_snapshot_roles_into_schema_enum() {
    for (role, want) in [
        ("textbox", "textbox"),
        ("searchbox", "textbox"),
        ("spinbutton", "textbox"),
        ("checkbox", "checkbox"),
        ("radio", "radio"),
        ("combobox", "combobox"),
        ("slider", "slider"),
        ("button", "textbox"),
    ] {
        assert_eq!(verify_value_type(Some(role)), want, "{role}");
    }
    assert_eq!(verify_value_type(None), "textbox");
}

#[tokio::test]
async fn stdio_caller_handshakes_and_matches_ids() {
    let (client_w, server_r) = tokio::io::duplex(64 * 1024);
    let (server_w, client_r) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        let mut lines = BufReader::new(server_r).lines();
        let mut out = server_w;
        let mut seen = Vec::new();
        while let Some(line) = lines.next_line().await.unwrap() {
            let msg: Value = serde_json::from_str(&line).unwrap();
            seen.push(msg["method"].as_str().unwrap().to_owned());
            let Some(id) = msg.get("id") else { continue };
            // Interleave a notification and noise before the real answer.
            let reply = format!(
                "{{\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\"}}\nnot json\n{}\n",
                json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": "hi"}]}})
            );
            out.write_all(reply.as_bytes()).await.unwrap();
        }
        seen
    });
    let mut caller = StdioCaller::connect(client_w, client_r).await.unwrap();
    let result = caller
        .call_tool("browser_snapshot", json!({}))
        .await
        .unwrap();
    assert_eq!(tool_text(&result), "hi");
    drop(caller);
    assert_eq!(
        server.await.unwrap(),
        ["initialize", "notifications/initialized", "tools/call"]
    );
}
