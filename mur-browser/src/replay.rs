//! `mur browser replay` core: drive a recorded [`Run`] against a fresh
//! Playwright MCP server with zero LLM calls (spec §6, layers L1/L2).
//!
//! Replay is its own MCP client — no agent sits on the other end. For each
//! step it either navigates (`goto`, after [`guard::check`]) or takes a
//! `browser_snapshot`, resolves the step's `locators[]` in priority order
//! against it via [`crate::locator`], and sends the step's tool with the
//! resolved `ref`. Self-healing (L3, `--heal`) is Task 5 and lives elsewhere.
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
            .with_context(|| {
                format!(
                    "no locator matched the page: [{}]",
                    step.locators.join(", ")
                )
            })?;
        if let Some(obj) = args.as_object_mut() {
            obj.insert("ref".into(), Value::String(reference));
        }
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

/// Spawn a headless, isolated Playwright MCP server and replay `run` on it.
/// `storage_state` is a decrypted profile state file the caller owns and
/// deletes afterwards.
pub async fn replay_live(
    run: &Run,
    allow: &[String],
    storage_state: Option<&std::path::Path>,
) -> Result<ReplayReport> {
    check_navigation(run, allow)?;
    // @playwright/mcp defaults to branded Google Chrome, which is often not
    // installed; the bundled Chromium is what `npx playwright install` provides.
    let mut args = vec![
        "--headless".to_owned(),
        "--isolated".to_owned(),
        "--browser=chromium".to_owned(),
    ];
    if let Some(path) = storage_state {
        args.push(format!("--storage-state={}", path.display()));
    }
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
        assert_eq!(click.1["ref"], "e7");
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
    async fn locator_miss_fails_with_candidates_listed() {
        let yaml = THREE.replace(
            "role:button[name=\"Sign in\"]",
            "role:button[name=\"Log in\"]",
        );
        let mut fake = Fake::default();
        let report = replay_with(&run(&yaml), &[], &mut fake).await.unwrap();
        assert_eq!(report.steps[1].status, StepStatus::Failed);
        let msg = report.steps[1].message.clone().unwrap();
        assert!(
            msg.contains("no locator matched") && msg.contains("testid:gone"),
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
