//! `mur browser replay` core: drive a recorded [`Run`] against a fresh
//! Playwright MCP server with zero LLM calls (spec §6, layers L1/L2).
//!
//! Replay is its own MCP client — no agent sits on the other end. For each
//! step it either navigates (`goto`, after [`guard::check`]) or takes a
//! `browser_snapshot`, resolves the step's `locators[]` in priority order
//! against it via [`crate::locator`], and sends the step's tool with the
//! resolved ref as `target` (the @playwright/mcp 0.0.82 argument name).
//! Self-healing (L3, `--heal`) is Task 5 and lives elsewhere.
//!
//! The transport is behind [`ToolCaller`] so the step loop is tested without
//! spawning `npx`; [`StdioCaller`] is the production line-JSON-RPC client.

use crate::{
    guard,
    locator::{self, Locator},
    recorder::{Action, Mode, Run, Step, call_for},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, Lines};

/// Outcome of one replayed step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Passed,
    Healed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepOutcome {
    pub step: u32,
    pub status: StepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator_used: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplayReport {
    pub run: String,
    pub total: u32,
    pub passed: u32,
    pub failed: u32,
    pub healed: u32,
    pub steps: Vec<StepOutcome>,
}

impl ReplayReport {
    fn new(run: &Run, steps: Vec<StepOutcome>) -> Self {
        let count = |s: StepStatus| steps.iter().filter(|o| o.status == s).count() as u32;
        Self {
            run: run.name.clone(),
            total: run.steps.len() as u32,
            passed: count(StepStatus::Passed),
            failed: count(StepStatus::Failed),
            healed: count(StepStatus::Healed),
            steps,
        }
    }

    /// Green / yellow / red, per spec §6.5.
    pub fn verdict(&self) -> &'static str {
        if self.failed > 0 {
            "red"
        } else if self.healed > 0 {
            "yellow"
        } else {
            "green"
        }
    }

    /// One-line summary for the CLI.
    pub fn summary(&self) -> String {
        format!(
            "{} {}: {}/{} passed, {} failed, {} healed",
            self.verdict(),
            self.run,
            self.passed,
            self.total,
            self.failed,
            self.healed
        )
    }
}

/// Reject the run before anything is spawned if any `goto` leaves the
/// profile's allowlist. Checking up front means a bad step 9 never lets
/// steps 1–8 act on an authenticated session first.
pub fn check_navigation(run: &Run, allow: &[String]) -> Result<()> {
    for step in run.steps.iter().filter(|s| s.action == Action::Goto) {
        let url = step
            .value
            .as_deref()
            .with_context(|| format!("step {} (goto) has no URL", step.step))?;
        guard::check(url, allow).with_context(|| format!("step {}", step.step))?;
    }
    Ok(())
}

/// `--dry-run`: validate navigation, report every step `Skipped`, spawn nothing.
pub fn dry_run(run: &Run, allow: &[String]) -> Result<ReplayReport> {
    check_navigation(run, allow)?;
    let steps = run
        .steps
        .iter()
        .map(|s| StepOutcome {
            step: s.step,
            status: StepStatus::Skipped,
            locator_used: None,
            message: None,
        })
        .collect();
    Ok(ReplayReport::new(run, steps))
}

/// Sends one MCP `tools/call` and returns the JSON-RPC `result` object.
pub trait ToolCaller {
    fn call_tool(
        &mut self,
        name: &str,
        arguments: Value,
    ) -> impl std::future::Future<Output = Result<Value>> + Send;
}

/// Replay `run` over `caller`. Navigation is guard-checked before the first
/// call. In `mode: test` the first failure stops the run (rest `Skipped`); in
/// `mode: automation` a failed assertion is logged but not fatal (spec §3).
pub async fn replay_with<C: ToolCaller + Send>(
    run: &Run,
    allow: &[String],
    caller: &mut C,
) -> Result<ReplayReport> {
    check_navigation(run, allow)?;
    let mut outcomes = Vec::with_capacity(run.steps.len());
    let mut stopped = false;
    for step in &run.steps {
        if stopped {
            outcomes.push(skipped(step, None));
            continue;
        }
        let outcome = match run_step(step, caller).await {
            Ok(locator_used) => StepOutcome {
                step: step.step,
                status: StepStatus::Passed,
                locator_used,
                message: None,
            },
            Err(error) if run.mode == Mode::Automation && step.action.is_assert() => skipped(
                step,
                Some(format!(
                    "assertion not enforced in automation mode: {error:#}"
                )),
            ),
            Err(error) => {
                stopped = true;
                StepOutcome {
                    step: step.step,
                    status: StepStatus::Failed,
                    locator_used: None,
                    message: Some(format!("{} — {error:#}", step.intent)),
                }
            }
        };
        outcomes.push(outcome);
    }
    Ok(ReplayReport::new(run, outcomes))
}

fn skipped(step: &Step, message: Option<String>) -> StepOutcome {
    StepOutcome {
        step: step.step,
        status: StepStatus::Skipped,
        locator_used: None,
        message,
    }
}

/// Run one step; returns the locator that hit (if the step needed one).
async fn run_step<C: ToolCaller + Send>(step: &Step, caller: &mut C) -> Result<Option<String>> {
    let (tool, mut args) = call_for(step)?;
    let mut hit = None;
    if step.action.needs_locator() {
        let snapshot = caller.call_tool("browser_snapshot", json!({})).await?;
        let nodes = locator::parse_snapshot(&tool_text(ensure_ok("browser_snapshot", &snapshot)?));
        let (locator, reference) = step
            .locators
            .iter()
            .find_map(|raw| {
                let parsed = Locator::parse(raw).ok()?;
                locator::resolve(&parsed, &nodes).map(|r| (raw.clone(), r))
            })
            // Real Playwright snapshots never carry data-testid, so a testid
            // cannot hit above. Only when nothing verifiable hit, hand the
            // first testid to Playwright as a selector; 0.0.82 resolves
            // non-ref `target`s itself and errors if nothing matches.
            .or_else(|| {
                step.locators
                    .iter()
                    .find_map(|raw| match Locator::parse(raw) {
                        Ok(Locator::TestId(id)) => Some((raw.clone(), testid_selector(&id))),
                        _ => None,
                    })
            })
            .with_context(|| {
                format!(
                    "no locator matched the page: [{}]",
                    step.locators.join(", ")
                )
            })?;
        // The snapshot node behind the ref; `None` for a testid selector.
        let node = nodes.iter().find(|n| n.reference == reference);
        args = assert_args(step, args, node, reference)?;
        hit = Some(locator);
    }
    // `browser_select_option` takes an array; `call_for` stores the scalar.
    if step.action == Action::Select
        && let Some(values) = args.get_mut("values")
        && let Value::String(one) = values.take()
    {
        *values = json!([one]);
    }
    let response = caller.call_tool(&tool, args).await?;
    ensure_ok(&tool, &response)?;
    Ok(hit)
}

/// Shape the final `arguments` for @playwright/mcp 0.0.82. Non-assert
/// actions get the resolved `target`; each `browser_verify_*` tool has its
/// own schema, so asserts are rebuilt from scratch.
fn assert_args(
    step: &Step,
    args: Value,
    node: Option<&locator::SnapshotNode>,
    reference: String,
) -> Result<Value> {
    let value = args.get("value").or_else(|| args.get("text")).cloned();
    Ok(match step.action {
        // { role, accessibleName } — Playwright runs getByRole itself.
        Action::AssertVisible => {
            let node = node.with_context(|| {
                "assert_visible needs a role locator that is in the snapshot; \
                 a testid selector has no role/accessible name to verify"
            })?;
            json!({"role": node.role, "accessibleName": node.name})
        }
        // { text } — the locator only gated that the text is on the page.
        Action::AssertText => json!({"text": value}),
        // { type, element, target, value }
        Action::AssertValue => json!({
            "type": verify_value_type(node.map(|n| n.role.as_str())),
            "element": step.intent,
            "target": reference,
            "value": value,
        }),
        _ => {
            let mut args = args;
            if let Some(obj) = args.as_object_mut() {
                // 0.0.82 names the element argument `target` (a snapshot ref
                // like `e7` or a selector); `ref` is rejected.
                obj.insert("target".into(), Value::String(reference));
            }
            args
        }
    })
}

/// Map a snapshot role onto `browser_verify_value`'s `type` enum
/// (`textbox|checkbox|radio|combobox|slider`). Anything else — including an
/// unknown role from a testid selector — is read via `inputValue`, i.e. textbox.
fn verify_value_type(role: Option<&str>) -> &'static str {
    match role {
        Some("checkbox") => "checkbox",
        Some("radio") => "radio",
        Some("combobox") => "combobox",
        Some("slider") => "slider",
        _ => "textbox",
    }
}

/// CSS attribute selector for a `data-testid`, quoted and escaped.
fn testid_selector(id: &str) -> String {
    let escaped = id.replace('\\', "\\\\").replace('"', "\\\"");
    format!("[data-testid=\"{escaped}\"]")
}

/// MCP tool failures arrive as successful results with `isError: true`.
fn ensure_ok<'a>(tool: &str, result: &'a Value) -> Result<&'a Value> {
    if result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        bail!("{tool} failed: {}", tool_text(result));
    }
    Ok(result)
}

/// Concatenate the `text` parts of an MCP tool result.
fn tool_text(result: &Value) -> String {
    result
        .get("content")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Sequential line-delimited JSON-RPC client over a server's stdio. Replay
/// issues one request at a time, so no id multiplexing is needed.
pub struct StdioCaller<W, R> {
    writer: W,
    lines: Lines<BufReader<R>>,
    next_id: u64,
}

impl<W, R> StdioCaller<W, R>
where
    W: AsyncWrite + Unpin + Send,
    R: AsyncRead + Unpin + Send,
{
    /// Perform the MCP `initialize` handshake.
    pub async fn connect(writer: W, reader: R) -> Result<Self> {
        let mut caller = Self {
            writer,
            lines: BufReader::new(reader).lines(),
            next_id: 1,
        };
        caller
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "mur browser replay", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
            .await
            .context("initialize Playwright MCP session")?;
        caller
            .send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}))
            .await?;
        Ok(caller)
    }

    async fn send(&mut self, message: &Value) -> Result<()> {
        let mut line = serde_json::to_vec(message)?;
        line.push(b'\n');
        self.writer
            .write_all(&line)
            .await
            .context("write to MCP server")?;
        self.writer.flush().await.context("flush MCP server stdin")
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = format!("mur-replay-{}", self.next_id);
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await?;
        while let Some(line) = self.lines.next_line().await? {
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue; // stray non-JSON output
            };
            if message.get("id").and_then(Value::as_str) != Some(id.as_str()) {
                continue; // notifications or unrelated traffic
            }
            if let Some(error) = message.get("error") {
                bail!("{method} failed: {error}");
            }
            return message
                .get("result")
                .cloned()
                .with_context(|| format!("{method} returned neither result nor error"));
        }
        bail!("MCP server exited before answering {method}")
    }
}

impl<W, R> ToolCaller for StdioCaller<W, R>
where
    W: AsyncWrite + Unpin + Send,
    R: AsyncRead + Unpin + Send,
{
    async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
        .await
    }
}

/// Launch arguments for the headless Playwright MCP server used by replay.
fn live_args(storage_state: Option<&std::path::Path>) -> Vec<String> {
    // @playwright/mcp defaults to branded Google Chrome, which is often not
    // installed; the bundled Chromium is what `npx playwright install` provides.
    let mut args = vec![
        "--headless".to_owned(),
        "--isolated".to_owned(),
        "--browser=chromium".to_owned(),
        // browser_verify_* (assert steps) are opt-in in @playwright/mcp.
        "--caps=testing".to_owned(),
    ];
    if let Some(path) = storage_state {
        args.push(format!("--storage-state={}", path.display()));
    }
    args
}

/// Spawn a headless, isolated Playwright MCP server and replay `run` on it.
/// `storage_state` is a decrypted profile state file the caller owns and
/// deletes afterwards.
pub async fn replay_live(
    run: &Run,
    allow: &[String],
    storage_state: Option<&std::path::Path>,
) -> Result<ReplayReport> {
    check_navigation(run, allow)?;
    let args = live_args(storage_state);
    let mut child = crate::proxy::playwright_command(&args)
        .spawn()
        .context("spawn Playwright MCP server (is `npx` on PATH and in the spawn allowlist?)")?;
    let stdin = child.stdin.take().context("child stdin")?;
    let stdout = child.stdout.take().context("child stdout")?;
    let result = async {
        let mut caller = StdioCaller::connect(stdin, stdout).await?;
        replay_with(run, allow, &mut caller).await
    }
    .await;
    let _ = child.kill().await;
    result
}

#[cfg(test)]
mod tests {

    #[test]
    fn live_args_enable_testing_caps_for_assert_steps() {
        // browser_verify_* tools are opt-in via --caps=testing in @playwright/mcp.
        let args = live_args(None);
        assert!(args.contains(&"--caps=testing".to_owned()), "{args:?}");
        assert!(args.contains(&"--browser=chromium".to_owned()));
        let with_state = live_args(Some(std::path::Path::new("/tmp/s.json")));
        assert!(with_state.contains(&"--storage-state=/tmp/s.json".to_owned()));
    }
    use super::*;
    use crate::recorder::from_yaml;

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
        let err = replay_with(&run(THREE), &["other.test".into()], &mut fake).await;
        assert!(err.is_err());
        assert!(fake.calls.is_empty(), "{:?}", fake.calls);
    }

    #[tokio::test]
    async fn resolves_locators_in_order_and_sends_ref() {
        let mut fake = Fake::default();
        let report = replay_with(&run(THREE), &["example.com".into()], &mut fake)
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
        let report = replay_with(&run(THREE), &[], &mut fake).await.unwrap();
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
        let report = replay_with(&run(&yaml), &[], &mut fake).await.unwrap();
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
        let report = replay_with(&run(THREE), &[], &mut fake).await.unwrap();
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
        let report = replay_with(&run(&yaml), &[], &mut fake).await.unwrap();
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
        let report = replay_with(&run(&yaml), &[], &mut fake).await.unwrap();
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
        replay_with(&run(yaml), &[], &mut fake).await.unwrap();
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
        let report = replay_with(&run(&yaml), &[], &mut fake).await.unwrap();
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
}
