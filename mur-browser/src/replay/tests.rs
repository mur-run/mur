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

#[test]
fn live_runs_are_never_replayed() {
    // Live sessions are interactive (spec: "not replayed and heal does not
    // apply"); every entry point funnels through check_navigation.
    let live = run(&THREE.replace("mode: test", "mode: live"));
    assert_eq!(live.mode, Mode::Live);
    let err = dry_run(&live, &[]).unwrap_err().to_string();
    assert!(err.contains("live"), "{err}");
    assert!(check_navigation(&live, &[]).is_err());
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
    /// Answers `browser_snapshot` instead of `SNAPSHOT` when set.
    snapshot: Option<&'static str>,
}

impl ToolCaller for Fake {
    async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        self.calls.push((name.to_owned(), arguments));
        let text = if name == "browser_snapshot" {
            self.snapshot.unwrap_or(SNAPSHOT)
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

/// A stale locator on an `assert_text` step must not fail the step: replay
/// only checks the text is visible, so it should not look the element up.
#[tokio::test]
async fn assert_text_ignores_stale_locator() {
    let yaml = "name: s\nmode: test\nrecorded_at: 2026-09-23T00:00:00Z\nsteps:\n- step: 1\n  intent: 確認歡迎文字\n  action: assert_text\n  value: Welcome\n  locators: ['testid:gone']\n";
    let mut fake = Fake::default();
    let report = replay_with(&run(yaml), &[], ReplayOptions::default(), &mut fake)
        .await
        .unwrap();
    assert_eq!(report.passed, 1, "{report:?}");
    assert!(
        fake.calls.iter().all(|(n, _)| n != "browser_snapshot"),
        "{:?}",
        fake.calls
    );
}

/// One `click` step whose only locator is `role:button[name="Sign in"]`.
const CLICK_SIGN_IN: &str = "name: s\nmode: test\nrecorded_at: 2026-09-23T00:00:00Z\nsteps:\n- step: 1\n  intent: 點擊登入按鈕\n  action: click\n  locators: ['role:button[name=\"Sign in\"]']\n";

/// Two nodes that `CLICK_SIGN_IN`'s locator matches equally well.
const TWO_SIGN_IN: &str = "- button \"Sign in\" [ref=e7]\n- button \"Sign in\" [ref=e8]";

/// A locator that matches several nodes still replays on the first one, as
/// before, but the step carries a warning and the summary prints it. The
/// verdict does not change: this is a warning, not a failure.
#[tokio::test]
async fn ambiguous_locator_passes_with_warning() {
    let mut fake = Fake {
        snapshot: Some(TWO_SIGN_IN),
        ..Fake::default()
    };
    let report = replay_with(
        &run(CLICK_SIGN_IN),
        &[],
        ReplayOptions::default(),
        &mut fake,
    )
    .await
    .unwrap();
    assert_eq!((report.passed, report.failed), (1, 0), "{report:?}");
    assert_eq!(report.verdict(), "green");
    let click = fake
        .calls
        .iter()
        .find(|(n, _)| n == "browser_click")
        .unwrap();
    assert_eq!(click.1["target"], "e7", "still acts on the first match");
    assert_eq!(
        report.steps[0].warning.as_deref(),
        Some("role:button[name=\"Sign in\"] matched 2 elements; replay used the first (e7)")
    );
    let summary = report.summary();
    assert!(
        summary.ends_with(
            "\nwarning: step 1 — role:button[name=\"Sign in\"] matched 2 elements; replay used the first (e7)"
        ),
        "{summary}"
    );
}

/// An ambiguous target is the likeliest reason a step fails, so the warning
/// is kept on a failed step too. The failure itself is unchanged.
#[tokio::test]
async fn ambiguous_locator_warning_survives_a_failed_step() {
    let mut fake = Fake {
        snapshot: Some(TWO_SIGN_IN),
        fail_tool: Some("browser_click"),
        ..Fake::default()
    };
    let report = replay_with(
        &run(CLICK_SIGN_IN),
        &[],
        ReplayOptions::default(),
        &mut fake,
    )
    .await
    .unwrap();
    assert_eq!(report.steps[0].status, StepStatus::Failed, "{report:?}");
    assert_eq!(report.verdict(), "red");
    let warning = report.steps[0]
        .warning
        .as_deref()
        .expect("warning kept on a failed step");
    assert!(warning.contains("matched 2 elements"), "{warning}");
    assert!(report.summary().contains("\nwarning: step 1 — "));
}

/// Only the locator replay actually used is counted: a later candidate that
/// would be ambiguous (`text:Sign` hits "Sign in" and "Sign up") is never
/// reached once the role locator hits a single node.
#[tokio::test]
async fn unique_hit_has_no_warning_even_if_a_later_candidate_is_ambiguous() {
    let yaml = CLICK_SIGN_IN.replace(
        "locators: ['role:button[name=\"Sign in\"]']",
        "locators: ['role:button[name=\"Sign in\"]', 'text:Sign']",
    );
    let mut fake = Fake {
        snapshot: Some("- button \"Sign in\" [ref=e7]\n- link \"Sign up\" [ref=e8]"),
        ..Fake::default()
    };
    let report = replay_with(&run(&yaml), &[], ReplayOptions::default(), &mut fake)
        .await
        .unwrap();
    assert_eq!(report.passed, 1, "{report:?}");
    assert_eq!(
        report.steps[0].locator_used.as_deref(),
        Some("role:button[name=\"Sign in\"]")
    );
    assert!(report.steps[0].warning.is_none(), "{report:?}");
    assert!(
        !report.summary().contains("warning"),
        "{}",
        report.summary()
    );
}

/// The page the step below was recorded on: one generic carrying "hello".
const RECORDED_GENERIC: &str = "- generic [ref=e1]: hello\n";

/// At replay another generic sits before it, so a bare `role:generic` would
/// hit `e1`: the wrong element.
const TWO_GENERICS: &str = "- generic [ref=e1]: other\n- generic [ref=e2]: hello";

/// Locators come from `candidates_for_ref`, not by hand, so this guards the
/// recorded order: `text:hello` ranks first and replay clicks the right node
/// without an ambiguity warning.
#[tokio::test]
async fn recorded_generic_with_text_replays_via_text() {
    use crate::locator::{candidates_for_ref, parse_snapshot};
    let recorded = candidates_for_ref("e1", &parse_snapshot(RECORDED_GENERIC));
    assert_eq!(recorded, vec!["text:hello", "role:generic"]);
    let list = recorded
        .iter()
        .map(|l| format!("'{l}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let yaml = CLICK_SIGN_IN.replace(
        "locators: ['role:button[name=\"Sign in\"]']",
        &format!("locators: [{list}]"),
    );
    let mut fake = Fake {
        snapshot: Some(TWO_GENERICS),
        ..Fake::default()
    };
    let report = replay_with(&run(&yaml), &[], ReplayOptions::default(), &mut fake)
        .await
        .unwrap();
    assert_eq!(report.passed, 1, "{report:?}");
    assert_eq!(report.steps[0].locator_used.as_deref(), Some("text:hello"));
    let click = fake
        .calls
        .iter()
        .find(|(n, _)| n == "browser_click")
        .unwrap();
    assert_eq!(click.1["target"], "e2", "the node that carries the text");
    assert!(report.steps[0].warning.is_none(), "{report:?}");
}

/// Reports written before this field existed still load, and a step without
/// a warning does not grow a `warning` key.
#[test]
fn step_outcome_without_warning_round_trips() {
    let outcome: StepOutcome = serde_yaml::from_str("step: 1\nstatus: passed\n").unwrap();
    assert!(outcome.warning.is_none());
    assert!(!serde_yaml::to_string(&outcome).unwrap().contains("warning"));
}

/// `browser_press_key` (0.0.82) takes only `{ key }` and sends it to whatever
/// has focus, so no element was picked: "replay used the first" would be false.
#[tokio::test]
async fn ambiguous_press_locator_has_no_warning() {
    let yaml = CLICK_SIGN_IN.replace("action: click", "action: press\n  value: Enter");
    let mut fake = Fake {
        snapshot: Some(TWO_SIGN_IN),
        ..Fake::default()
    };
    let report = replay_with(&run(&yaml), &[], ReplayOptions::default(), &mut fake)
        .await
        .unwrap();
    assert_eq!(report.passed, 1, "{report:?}");
    assert!(
        fake.calls.iter().any(|(n, _)| n == "browser_press_key"),
        "{:?}",
        fake.calls
    );
    assert!(report.steps[0].warning.is_none(), "{report:?}");
}
