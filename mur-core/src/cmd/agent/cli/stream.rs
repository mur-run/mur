//! Streaming bridge between the blocking A2A client and the async TUI loop.
//!
//! `dial_message_streaming` (see [`crate::a2a_dial`]) is synchronous and blocks
//! its thread until the task completes, invoking `FnMut` callbacks for token
//! deltas and HITL prompts. We run it on a `spawn_blocking` worker and forward
//! every event over an mpsc channel as a [`StreamMsg`], so the UI's
//! `tokio::select!` loop stays responsive. HITL approval and cancellation are
//! issued on *separate* connections via `dial_method` (they must not wait on the
//! streaming socket, which is busy reading) — mirroring the MUR Hub design.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::a2a_dial::{DialMode, dial_message_streaming, dial_method};

/// Bounded capacity of the worker→UI event channel. Deltas are tiny and the UI
/// drains them every frame; this only bounds a runaway burst.
pub const STREAM_CHANNEL_CAP: usize = 512;

/// One event from the streaming worker to the UI loop. Every turn-scoped
/// variant carries the originating `task_id` so the UI can drop late events
/// from a cancelled / superseded worker (the channel is shared across turns).
#[derive(Debug)]
pub enum StreamMsg {
    /// A token (or token batch) of the agent's reply. `thinking` marks
    /// reasoning tokens, which the UI renders dimmed.
    Delta {
        task_id: String,
        text: String,
        thinking: bool,
    },
    /// The agent paused to ask the user to approve a tool call.
    Hitl { task_id: String, req: HitlRequest },
    /// The task finished; carries the final A2A Task value.
    Done { task_id: String, task: Box<Value> },
    /// The dial failed (or the agent errored). Carries a display string.
    Err { task_id: String, error: String },
    /// An out-of-band notice not tied to a turn (e.g. a failed HITL/cancel
    /// dial). Always shown regardless of the active turn.
    Note(String),
    /// The in-flight turn no longer exists on the runtime (it restarted and
    /// tasks live in memory only, or it stopped). The UI must drop the dead
    /// task binding; `resend` carries user text (a failed steer) to replay as
    /// a fresh `message/send` on the same channel so it is not lost.
    TurnLost {
        task_id: String,
        note: String,
        resend: Option<String>,
    },
    /// An approval decision arrived after the gate had already auto-denied at
    /// timeout (#8). Turn-independent like `Note`. `retry` carries the user's
    /// last message so the composer can offer a one-key re-run of the request
    /// that stranded the expired call.
    Expired { tool: String, retry: Option<String> },
    /// A chunk of a running local `!command`'s output (stdout and stderr
    /// interleaved in arrival order, as a terminal shows them). `gen_id` is the
    /// shell generation it belongs to; the UI drops a retired one (D8).
    /// Turn-independent like `Note`.
    ShellOutput { gen_id: u64, chunk: String },
    /// A local `!command` ended. The output is not here — it was streamed,
    /// and the live card owns it. Guaranteed to arrive after every chunk the
    /// command produced (D9).
    ShellDone {
        gen_id: u64,
        cmd: String,
        end: super::shell::ShellEnd,
    },
    /// An argv-launched slash command ended. Its output stays in the shell
    /// card and is never forwarded to the conversational agent.
    ShellCardDone {
        gen_id: u64,
        end: super::shell::ShellEnd,
    },
    /// A tool call started running (name + args).
    StepStarted {
        task_id: String,
        step_id: String,
        name: String,
        args: Value,
    },
    /// A tool call finished (result/error + duration).
    StepCompleted {
        task_id: String,
        step_id: String,
        ok: bool,
        output: String,
        truncated: bool,
        full_len: usize,
        error: Option<String>,
        duration_ms: u64,
        /// The sandbox refused to run this call. Distinct from a legitimate
        /// non-zero exit (`ok: false`); an older runtime that omits this key
        /// is treated as not-denied.
        denied: bool,
        /// The call yielded and the command is still running.
        running: bool,
    },
    /// Post-hook token count for a finished call (`step/tokens`).
    StepTokens {
        task_id: String,
        step_id: String,
        tokens: usize,
    },
    /// The egress proxy blocked a host (#1907). Turn-independent: the chip
    /// stays useful after the turn that triggered it ends.
    EgressDenied(mur_common::egress_event::EgressDenied),
    /// The attached `/review` worker asks the UI something (P3b-§4.1).
    ReviewReq(crate::cmd::fleet::review::murmur::bridge::DriverReq),
    /// The attached `/review` worker ended; sent once, last (P3b-§7.2).
    ReviewFinished(crate::cmd::fleet::review::murmur::bridge::Outcome),
    /// Esc×2's `tasks/cancel` failed (P3b-§6.3 step 5).
    ReviewCancelFailed(String),
    /// A background manage op (`run_manage_bg`, #1921) finished; the error
    /// side is already display-formatted.
    ManageDone(Result<super::manage::Managed, String>),
}

impl StreamMsg {
    /// The turn this event belongs to, or `None` for turn-independent notices.
    pub fn task_id(&self) -> Option<&str> {
        match self {
            StreamMsg::Delta { task_id, .. }
            | StreamMsg::Hitl { task_id, .. }
            | StreamMsg::Done { task_id, .. }
            | StreamMsg::Err { task_id, .. }
            | StreamMsg::TurnLost { task_id, .. }
            | StreamMsg::StepStarted { task_id, .. }
            | StreamMsg::StepCompleted { task_id, .. }
            | StreamMsg::StepTokens { task_id, .. } => Some(task_id),
            StreamMsg::Note(_)
            | StreamMsg::EgressDenied(_)
            | StreamMsg::Expired { .. }
            | StreamMsg::ShellOutput { .. }
            | StreamMsg::ShellDone { .. }
            | StreamMsg::ShellCardDone { .. }
            | StreamMsg::ReviewReq(_)
            | StreamMsg::ReviewFinished(_)
            | StreamMsg::ReviewCancelFailed(_)
            | StreamMsg::ManageDone(_) => None,
        }
    }
}

/// A human-in-the-loop tool-approval request, parsed from a
/// `tool/approval_needed` notification's params.
#[derive(Debug, Clone)]
pub struct HitlRequest {
    pub hitl_id: String,
    /// The `step_id` of the card that ran this tool (P2: lets the cli show the
    /// approval inline on that card). `None` on runtimes predating the field.
    pub step_id: Option<String>,
    pub tool_name: String,
    pub tool_input: Value,
    /// When the runtime stops waiting for this decision: receipt time plus
    /// the `timeout_ms` the runtime sent with `tool/approval_needed`. The
    /// panel countdown, the status line, the wake timer and
    /// `expire_stale_hitl` all read this one value, so none of them can
    /// disagree with the others or with the runtime. Set once, at receipt —
    /// see [`deadline_from`].
    pub deadline: std::time::Instant,
    /// #1600: the strictest `risk:` the agent's profile declares for this
    /// tool, shipped by the runtime. `None` = no rule declares one, or a
    /// runtime predating the field. Only ever RAISES the tier — see [`Self::tier`].
    pub declared_risk: Option<mur_common::hitl::RiskTier>,
}

impl HitlRequest {
    /// The tier every CLI gate decides on: `max(classify, declared_risk)`.
    /// One function so the auto lanes, the grant row and the plain-mode
    /// approver cannot disagree about what a call is worth.
    pub fn tier(&self) -> mur_common::hitl::RiskTier {
        super::tool_tier::effective_tier(
            &self.tool_name,
            Some(&self.tool_input),
            self.declared_risk,
        )
    }

    /// Time left before the runtime denies this request on its own.
    pub fn remaining(&self, now: std::time::Instant) -> std::time::Duration {
        self.deadline.saturating_duration_since(now)
    }

    fn from_value(v: Value, deadline: std::time::Instant) -> Self {
        let tool_name = v
            .get("tool_name")
            .and_then(Value::as_str)
            .unwrap_or("tool")
            .to_string();
        Self {
            hitl_id: v
                .get("hitl_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            step_id: v.get("step_id").and_then(Value::as_str).map(str::to_string),
            tool_input: v.get("tool_input").cloned().unwrap_or(Value::Null),
            tool_name,
            deadline,
            declared_risk: v
                .get("risk")
                .and_then(|r| serde_json::from_value(r.clone()).ok()),
        }
    }

    /// One request per gated call. A P3 runtime sends `calls: [...]`; each
    /// entry has its own `hitl_id`. Older runtimes send only the top-level
    /// fields, which are exactly one call — so the fallback is one request.
    pub fn from_params(v: Value) -> Vec<Self> {
        // One deadline for the whole batch: the runtime sends `timeout_ms`
        // once, at the top level, and waits on every call against it.
        let deadline = deadline_from(&v, std::time::Instant::now());
        match v.get("calls").and_then(Value::as_array) {
            Some(calls) if !calls.is_empty() => calls
                .iter()
                .cloned()
                .map(|c| Self::from_value(c, deadline))
                .collect(),
            _ => vec![Self::from_value(v, deadline)],
        }
    }
}

/// The deadline for a request received at `received`: plus the runtime's
/// `timeout_ms`, or plus `DEFAULT_TIMEOUT` for a runtime predating the field.
///
/// The ONLY place the CLI names `DEFAULT_TIMEOUT`. The CLI's clock starts at
/// receipt, after the runtime's started at send, so the CLI always retires a
/// request at or after the runtime has already denied it: a decision sent in
/// that gap is refused by the runtime and nothing is approved late.
pub(crate) fn deadline_from(params: &Value, received: std::time::Instant) -> std::time::Instant {
    let timeout = params
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .map(std::time::Duration::from_millis)
        .unwrap_or(crate::hitl::gate::DEFAULT_TIMEOUT);
    received + timeout
}

/// Build the `message/send` params for one turn, threading the previous turn's
/// task id as `context.task_id` so the agent keeps conversation history.
/// `image`, when set, is `(mime, base64)` attached as a second A2A `data` part
/// (an inline screenshot or pasted image file) that the runtime forwards to the
/// model as a vision input. `cwd` is the user's working directory, sent as
/// `context.cwd` on EVERY turn: it used to be a line of text in the first
/// message only, and history is trimmed oldest-first, so long conversations
/// forgot which project they were in.
pub fn build_params(
    text: &str,
    task_id: &str,
    context_task_id: Option<&str>,
    image: Option<(&str, &str)>,
    cwd: Option<&Path>,
) -> Value {
    let mut parts = vec![json!({ "kind": "text", "text": text })];
    if let Some((mime, b64)) = image {
        parts.push(json!({
            "kind": "data",
            "mimeType": mime,
            "data": { "base64": b64 },
        }));
    }
    let mut params = json!({
        "message": { "role": "user", "parts": parts },
        "task_id": task_id,
    });
    if let Some(tid) = context_task_id {
        params["context"]["task_id"] = json!(tid);
    }
    if let Some(dir) = cwd {
        params["context"]["cwd"] = json!(dir.to_string_lossy());
    }
    params
}

/// Spawn the blocking streaming dial on a **detached OS thread**, forwarding
/// deltas, HITL prompts, and the terminal Done/Err event over `tx`, each tagged
/// with `task_id`.
///
/// A plain `std::thread` (not `tokio::task::spawn_blocking`) is deliberate: the
/// dial parks in a blocking socket read until the task completes, and a detached
/// thread is not tracked by the tokio runtime, so quitting the TUI mid-stream
/// cannot hang process shutdown waiting for it (the OS reclaims it on exit).
pub fn spawn_stream(
    home: PathBuf,
    agent: String,
    params: Value,
    task_id: String,
    tx: mpsc::Sender<StreamMsg>,
) {
    std::thread::spawn(move || {
        let tid = task_id.clone();
        let result = dial_message_streaming(
            &home,
            &agent,
            params,
            |delta, thinking, delta_task_id| {
                // The runtime already routes deltas to this connection only;
                // defensively drop any whose stamped id isn't this turn's (an
                // empty id means a pre-routing runtime — accept it).
                if !delta_task_id.is_empty() && delta_task_id != tid {
                    return;
                }
                let _ = tx.blocking_send(StreamMsg::Delta {
                    task_id: tid.clone(),
                    text: delta.to_string(),
                    thinking,
                });
            },
            |hitl| {
                for req in HitlRequest::from_params(hitl) {
                    let _ = tx.blocking_send(StreamMsg::Hitl {
                        task_id: tid.clone(),
                        req,
                    });
                }
            },
            |step| {
                let msg = match step {
                    crate::a2a_dial::StepEvent::Started {
                        step_id,
                        task_id,
                        name,
                        args,
                    } => StreamMsg::StepStarted {
                        task_id,
                        step_id,
                        name,
                        args,
                    },
                    crate::a2a_dial::StepEvent::Completed {
                        step_id,
                        task_id,
                        ok,
                        output,
                        truncated,
                        full_len,
                        error,
                        duration_ms,
                        denied,
                        running,
                    } => StreamMsg::StepCompleted {
                        task_id,
                        step_id,
                        ok,
                        output,
                        truncated,
                        full_len,
                        error,
                        duration_ms,
                        denied,
                        running,
                    },
                    crate::a2a_dial::StepEvent::Tokens {
                        step_id,
                        task_id,
                        tokens,
                    } => StreamMsg::StepTokens {
                        task_id,
                        step_id,
                        tokens,
                    },
                    crate::a2a_dial::StepEvent::EgressDenied(ev) => StreamMsg::EgressDenied(ev),
                };
                let _ = tx.blocking_send(msg);
            },
        );
        let _ = match result {
            Ok(task) => tx.blocking_send(StreamMsg::Done {
                task_id,
                task: Box::new(task),
            }),
            Err(e) => tx.blocking_send(StreamMsg::Err {
                task_id,
                error: format!("{e:#}"),
            }),
        };
    });
}

/// Params for `tool/hitl_respond`.
///
/// `surface` is the audit attribution recorded on the signed `HitlResponse`
/// (`mur_common::hitl::HitlResponse`): `"cli"` when a human answered the gate,
/// `"auto"` when the session answered it for them (`--auto`, the read lane, a
/// session-wide grant). Sending `"cli"` for a machine decision would claim a
/// human was there, which is the class of lie spec §4.4 exists to prevent.
///
/// Omitting the field is not harmless either: the runtime defaults a missing
/// surface to `"unknown"`, so the channel records that someone answered without
/// recording who. Build the params here rather than inline so both CLI senders
/// stay in step.
///
/// An allow also carries the home's approval token
/// (`mur_common::hitl::approval_token`); the runtime refuses an allow without
/// it. A token that cannot be read is not fatal here — the allow goes out bare
/// and the runtime's refusal names the cause.
pub(crate) fn hitl_respond_params(
    home: &Path,
    hitl_id: &str,
    allow: bool,
    surface: &str,
) -> serde_json::Value {
    let mut p = json!({ "hitl_id": hitl_id, "allow": allow, "surface": surface });
    if let Err(e) = mur_common::hitl::approval_token::attach(&mut p, home) {
        tracing::warn!(error = %e, "could not read the HITL approval token; sending the answer without it");
    }
    p
}

/// What a tool-approval prompt outside the TUI needs to show: the effective
/// tier (#1600: the declared `risk:` only raises it), whether that tier is
/// within the auto ceiling, and the same one-line summary plain mode prints.
/// One place, so `mur fleet review`'s prompt cannot drift from `murmur`'s.
pub(crate) fn tool_tier_and_summary(
    hitl: &serde_json::Value,
) -> (mur_common::hitl::RiskTier, bool, String) {
    let tool = hitl
        .get("tool_name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("tool");
    let declared = hitl
        .get("risk")
        .and_then(|r| serde_json::from_value(r.clone()).ok());
    let tier = super::tool_tier::effective_tier(tool, hitl.get("tool_input"), declared);
    let width = crossterm::terminal::size()
        .map(|(w, _)| w)
        .unwrap_or(super::call_summary::ASSUMED_WIDTH);
    let summary = super::call_summary::approval_summary(
        tool,
        hitl.get("tool_input").unwrap_or(&serde_json::Value::Null),
        width,
        super::call_summary::ApprovalFrame::Transcript,
    );
    (
        tier,
        mur_common::hitl::tier_may_be_granted(tier),
        super::scrub::scrub_str(&summary).into_owned(),
    )
}

/// Answer a pending HITL request on a fresh connection. Does not block the
/// streaming worker; the agent resumes once the runtime receives this.
pub async fn respond_hitl(
    home: PathBuf,
    agent: String,
    hitl_id: String,
    allow: bool,
    surface: &'static str,
) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        dial_method(
            &home,
            &agent,
            "tool/hitl_respond",
            hitl_respond_params(&home, &hitl_id, allow, surface),
            DialMode::RequireRunning,
        )
    })
    .await??;
    Ok(())
}

/// Inject a steering message into the in-flight turn on a fresh connection.
pub async fn steer_turn(
    home: PathBuf,
    agent: String,
    task_id: String,
    message: String,
) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        dial_method(
            &home,
            &agent,
            "turn/steer",
            json!({ "task_id": task_id, "message": message }),
            DialMode::RequireRunning,
        )
    })
    .await??;
    Ok(())
}

/// Cancel the in-flight task by id on a fresh connection. Best-effort: the
/// partial reply already streamed is kept by the caller.
pub async fn cancel_task(home: PathBuf, agent: String, task_id: String) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        dial_method(
            &home,
            &agent,
            "tasks/cancel",
            json!({ "id": task_id }),
            DialMode::RequireRunning,
        )
    })
    .await??;
    Ok(())
}

/// Extract the agent's reply text and the task id from a final Task value,
/// matching the Hub's logic: walk `messages` backwards for the most recent
/// `agent` message and concatenate its text parts.
pub fn extract_reply(task: &Value) -> (String, Option<String>) {
    let task_id = task.get("id").and_then(Value::as_str).map(str::to_string);
    let reply = task
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|msgs| {
            msgs.iter()
                .rev()
                .find(|m| m.get("role").and_then(Value::as_str) == Some("agent"))
        })
        .map(extract_text)
        .unwrap_or_default();
    (reply, task_id)
}

/// The restart (or other) chip the runtime attached to the final reply, if
/// any — see [`mur_common::proposal::PROPOSAL_MIME`].
pub fn attached_proposal(task: &Value) -> Option<mur_common::proposal::Proposal> {
    let reply = task
        .get("messages")?
        .as_array()?
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("agent"))?;
    mur_common::proposal::attached(reply.get("parts")?)
}

/// Interpret a final Task: `Ok((reply, task_id))` for a completed task, or
/// `Err(cause)` for a failed/cancelled one. The runtime returns failed tasks as
/// a *successful* JSON-RPC result with the cause in `task.error`, so callers
/// must inspect `state`/`error` rather than assuming `Ok` means success.
pub fn task_outcome(task: &Value) -> Result<(String, Option<String>), String> {
    let state = task.get("state").and_then(Value::as_str).unwrap_or("");
    if state == "failed" || state == "cancelled" {
        let cause = task
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("agent task {state}"));
        return Err(cause);
    }
    let (reply, task_id) = extract_reply(task);
    Ok((reply, task_id))
}

/// The id of a failed or cancelled task the runtime says it remembered
/// (`usage.remembered`, #2071) — the next turn may thread onto it. `None`
/// for an older runtime, which drops such turns: threading onto a key it
/// never stored would start the conversation over.
pub fn remembered_task_id(task: &Value) -> Option<String> {
    let remembered = task
        .get("usage")
        .and_then(|u| u.get("remembered"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !remembered {
        return None;
    }
    task.get("id").and_then(Value::as_str).map(str::to_string)
}

fn extract_text(message: &Value) -> String {
    message
        .get("parts")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod hitl_step_tests;
#[cfg(test)]
mod tests;
