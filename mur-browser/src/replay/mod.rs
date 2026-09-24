//! `mur browser replay` core: drive a recorded [`Run`] against a fresh
//! Playwright MCP server with zero LLM calls (spec §6, layers L1/L2).
//!
//! Replay is its own MCP client — no agent sits on the other end. For each
//! step it either navigates (`goto`, after [`guard::check`]) or takes a
//! `browser_snapshot`, resolves the step's `locators[]` in priority order
//! against it via [`crate::locator`], and sends the step's tool with the
//! resolved ref as `target` (the @playwright/mcp 0.0.82 argument name).
//! With [`ReplayOptions::heal`], an element step whose locators all miss is
//! matched offline by [`crate::heal::find_heal`] and must be confirmed by the
//! next element step hitting directly (design spec D3).
//!
//! The transport is behind [`ToolCaller`] so the step loop is tested without
//! spawning `npx`; [`StdioCaller`] is the production line-JSON-RPC client.

use crate::{
    guard,
    heal::{self, BudgetExceeded, HealEvent, HealStatus},
    locator::{self, Locator},
    recorder::{Action, Mode, Run, Step, call_for},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[cfg(test)]
mod heal_tests;
mod stdio;
#[cfg(test)]
mod tests;

pub use stdio::{StdioCaller, replay_live};

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
    /// Every heal attempted this run, with its D3 status.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub heals: Vec<HealEvent>,
    /// Set when a `mode: test` run healed past its budget (D4): red, and
    /// the CLI exits non-zero after writing this report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_exceeded: Option<BudgetExceeded>,
}

impl ReplayReport {
    fn new(run: &Run, steps: Vec<StepOutcome>, heals: Vec<HealEvent>) -> Self {
        let count = |s: StepStatus| steps.iter().filter(|o| o.status == s).count() as u32;
        Self {
            run: run.name.clone(),
            total: run.steps.len() as u32,
            passed: count(StepStatus::Passed),
            failed: count(StepStatus::Failed),
            healed: count(StepStatus::Healed),
            steps,
            heals,
            budget_exceeded: None,
        }
    }

    /// Green / yellow / red, per spec §6.5.
    pub fn verdict(&self) -> &'static str {
        if self.failed > 0 || self.budget_exceeded.is_some() {
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
    Ok(ReplayReport::new(run, steps, Vec::new()))
}

/// Knobs for [`replay_with`].
#[derive(Debug, Clone, Copy)]
pub struct ReplayOptions {
    /// Try an offline heal when every locator of an element step misses.
    pub heal: bool,
    /// Heal budget as a share of element steps; enforced in `mode: test` only.
    pub max_heal_ratio: f32,
}

impl ReplayOptions {
    /// Heal off, default budget.
    pub const DEFAULT: Self = Self {
        heal: false,
        max_heal_ratio: heal::DEFAULT_HEAL_RATIO,
    };
}

impl Default for ReplayOptions {
    fn default() -> Self {
        Self::DEFAULT
    }
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
///
/// A heal stays `Pending` until the next element step: a direct hit verifies
/// it; a failure, a second heal, or any failure in between rolls it back,
/// marks the healed step `Failed`, and stops the run. Still `Pending` at the
/// end → `Unverified`.
///
/// In `mode: test`, heals that were not rolled back are held to
/// `opts.max_heal_ratio` of the element steps; going over still returns
/// `Ok`, with [`ReplayReport::budget_exceeded`] set (D4).
pub async fn replay_with<C: ToolCaller + Send>(
    run: &Run,
    allow: &[String],
    opts: ReplayOptions,
    caller: &mut C,
) -> Result<ReplayReport> {
    check_navigation(run, allow)?;
    let mut outcomes: Vec<StepOutcome> = Vec::with_capacity(run.steps.len());
    let mut heals: Vec<HealEvent> = Vec::new();
    // (index into `outcomes`, index into `heals`) of the heal awaiting proof.
    let mut pending: Option<(usize, usize)> = None;
    let mut stopped = false;
    for step in &run.steps {
        if stopped {
            outcomes.push(skipped(step, None));
            continue;
        }
        let outcome = match run_step(step, opts, caller).await {
            Ok(hit) => {
                let status = if hit.heal.is_some() {
                    StepStatus::Healed
                } else {
                    StepStatus::Passed
                };
                if let Some(p) = pending.take() {
                    if hit.heal.is_some() {
                        // Two heals cannot vouch for each other.
                        let why = format!(
                            "step {} also needed a heal, so neither is verified",
                            step.step
                        );
                        roll_back(&mut outcomes, &mut heals, p, &why);
                        stopped = true;
                    } else if heal::is_element_step(step.action) {
                        heals[p.1].status = HealStatus::Verified;
                        log_heal(&heals[p.1]);
                    } else {
                        pending = Some(p);
                    }
                }
                let mut outcome = StepOutcome {
                    step: step.step,
                    status,
                    locator_used: hit.locator,
                    message: None,
                };
                if let Some(mut event) = hit.heal {
                    outcome.message = Some(format!(
                        "healed onto {} (score {:.2})",
                        event.node, event.score
                    ));
                    if stopped {
                        event.status = HealStatus::RolledBack;
                        outcome.status = StepStatus::Failed;
                        outcome.message = Some(format!(
                            "{} — healed onto {}, but the previous heal was rolled back",
                            step.intent, event.node
                        ));
                    } else {
                        pending = Some((outcomes.len(), heals.len()));
                    }
                    log_heal(&event);
                    heals.push(event);
                }
                outcome
            }
            Err(error) => {
                // Any failure while a heal awaits proof makes the page state
                // untrustworthy, even an assert automation would let slide.
                if let Some(p) = pending.take() {
                    let why = format!("step {} failed: {error:#}", step.step);
                    roll_back(&mut outcomes, &mut heals, p, &why);
                    stopped = true;
                }
                if !stopped && run.mode == Mode::Automation && step.action.is_assert() {
                    skipped(
                        step,
                        Some(format!(
                            "assertion not enforced in automation mode: {error:#}"
                        )),
                    )
                } else {
                    stopped = true;
                    StepOutcome {
                        step: step.step,
                        status: StepStatus::Failed,
                        locator_used: None,
                        message: Some(format!("{} — {error:#}", step.intent)),
                    }
                }
            }
        };
        tracing::debug!(
            step = outcome.step,
            action = ?step.action,
            status = ?outcome.status,
            locator = outcome.locator_used.as_deref().unwrap_or(""),
            message = outcome.message.as_deref().unwrap_or(""),
            "replayed step"
        );
        outcomes.push(outcome);
    }
    if let Some((_, h)) = pending {
        heals[h].status = HealStatus::Unverified;
        log_heal(&heals[h]);
    }
    let mut report = ReplayReport::new(run, outcomes, heals);
    if run.mode == Mode::Test {
        report.budget_exceeded = check_budget(run, &report.heals, opts.max_heal_ratio);
    }
    Ok(report)
}

/// D4: denominator is element steps; rolled-back heals already failed their
/// step, so they do not count against the budget.
fn check_budget(run: &Run, heals: &[HealEvent], max_ratio: f32) -> Option<BudgetExceeded> {
    let total = run
        .steps
        .iter()
        .filter(|s| heal::is_element_step(s.action))
        .count() as u32;
    let healed = heals
        .iter()
        .filter(|h| h.status != HealStatus::RolledBack)
        .count() as u32;
    let over = BudgetExceeded::check(healed, total, max_ratio);
    if let Some(b) = &over {
        tracing::info!(
            healed = b.healed,
            total = b.total,
            allowed = b.allowed,
            max_ratio = b.max_ratio,
            "heal budget exceeded"
        );
    }
    over
}

/// Undo a pending heal: the event is `RolledBack` and its step `Failed`.
fn roll_back(
    outcomes: &mut [StepOutcome],
    heals: &mut [HealEvent],
    (o, h): (usize, usize),
    why: &str,
) {
    let event = &mut heals[h];
    event.status = HealStatus::RolledBack;
    let outcome = &mut outcomes[o];
    outcome.status = StepStatus::Failed;
    outcome.message = Some(format!(
        "healed onto {} (score {:.2}), rolled back: {why}",
        event.node, event.score
    ));
    log_heal(event);
}

fn log_heal(event: &HealEvent) {
    tracing::info!(
        step = event.step,
        status = ?event.status,
        score = event.score,
        node = event.node.as_str(),
        "browser replay heal"
    );
}

fn skipped(step: &Step, message: Option<String>) -> StepOutcome {
    StepOutcome {
        step: step.step,
        status: StepStatus::Skipped,
        locator_used: None,
        message,
    }
}

/// A step that ran: the locator that hit, and the heal it took, if any.
struct Hit {
    locator: Option<String>,
    heal: Option<HealEvent>,
}

/// Why a step failed. Only `LocateMiss` may be healed: once anything has
/// been sent to Playwright (an action, or a testid fallback selector), a
/// failure means the page refused it, not that the recording is stale.
#[derive(Debug)]
enum StepError {
    /// Nothing in `locators[]` resolved and there was no testid to fall back on.
    LocateMiss(anyhow::Error),
    /// Everything else: snapshot, argument shaping, or the tool call itself.
    Action(anyhow::Error),
}

impl From<anyhow::Error> for StepError {
    fn from(error: anyhow::Error) -> Self {
        StepError::Action(error)
    }
}

impl std::fmt::Display for StepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StepError::LocateMiss(e) | StepError::Action(e) => write!(f, "{e:#}"),
        }
    }
}

/// Run one step. A `LocateMiss` on an element step is healed here when
/// `opts.heal` is on; the healed ref is used for this call only.
async fn run_step<C: ToolCaller + Send>(
    step: &Step,
    opts: ReplayOptions,
    caller: &mut C,
) -> Result<Hit, StepError> {
    let (tool, mut args) = call_for(step)?;
    let mut hit = Hit {
        locator: None,
        heal: None,
    };
    if step.action.needs_locator() {
        let snapshot = caller.call_tool("browser_snapshot", json!({})).await?;
        let nodes = locator::parse_snapshot(&tool_text(ensure_ok("browser_snapshot", &snapshot)?));
        let reference = match resolve_target(step, &nodes) {
            Ok((locator, reference)) => {
                hit.locator = Some(locator);
                reference
            }
            Err(StepError::LocateMiss(miss)) if opts.heal && heal::is_element_step(step.action) => {
                let found = heal::find_heal(step, &nodes).map_err(|declined| {
                    StepError::LocateMiss(miss.context(format!("heal declined: {declined}")))
                })?;
                hit.locator = found.to.first().cloned();
                hit.heal = Some(HealEvent {
                    step: step.step,
                    from: step.locators.clone(),
                    to: found.to,
                    node: found.chosen.node,
                    score: found.chosen.score,
                    reason: format!(
                        "no locator matched the page: [{}]",
                        step.locators.join(", ")
                    ),
                    status: HealStatus::Pending,
                });
                found.reference
            }
            Err(e) => return Err(e),
        };
        // The snapshot node behind the ref; `None` for a testid selector.
        let node = nodes.iter().find(|n| n.reference == reference);
        args = assert_args(step, args, node, reference)?;
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

/// Resolve `locators[]` in priority order against the snapshot; returns the
/// raw locator that hit and the `target` to send.
fn resolve_target(
    step: &Step,
    nodes: &[locator::SnapshotNode],
) -> Result<(String, String), StepError> {
    step.locators
        .iter()
        .find_map(|raw| {
            let parsed = Locator::parse(raw).ok()?;
            locator::resolve(&parsed, nodes).map(|r| (raw.clone(), r))
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
        .ok_or_else(|| {
            StepError::LocateMiss(anyhow::anyhow!(
                "no locator matched the page: [{}]",
                step.locators.join(", ")
            ))
        })
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
