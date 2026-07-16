//! Streaming bridge between the blocking A2A client and the async TUI loop.
//!
//! `dial_message_streaming` (see [`crate::a2a_dial`]) is synchronous and blocks
//! its thread until the task completes, invoking `FnMut` callbacks for token
//! deltas and HITL prompts. We run it on a `spawn_blocking` worker and forward
//! every event over an mpsc channel as a [`StreamMsg`], so the UI's
//! `tokio::select!` loop stays responsive. HITL approval and cancellation are
//! issued on *separate* connections via `dial_method` (they must not wait on the
//! streaming socket, which is busy reading) — mirroring the MUR Hub design.

use std::path::PathBuf;

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
    /// A local `!command` finished. Turn-independent like `Note`.
    ShellDone { cmd: String, output: String },
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
    },
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
            | StreamMsg::StepCompleted { task_id, .. } => Some(task_id),
            StreamMsg::Note(_) | StreamMsg::ShellDone { .. } => None,
        }
    }
}

/// Cap on captured `!command` output forwarded to the transcript/agent.
pub const SHELL_MAX_BYTES: usize = 8 * 1024;
/// Wall-clock limit for a `!command`.
pub const SHELL_TIMEOUT_SECS: u64 = 30;

/// Run a local `!command` through the user's shell (`$SHELL -c`, fallback
/// `/bin/sh`; `cmd /C` on Windows), merging stderr into stdout, with a
/// timeout and output cap.
pub async fn run_local_shell(cmd: String) -> String {
    use tokio::process::Command;
    #[cfg(unix)]
    let (shell, flag) = (
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
        "-c",
    );
    #[cfg(windows)]
    let (shell, flag) = (
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd".into()),
        "/C",
    );
    let fut = Command::new(shell)
        .arg(flag)
        .arg(&cmd)
        .kill_on_drop(true)
        .output();
    let out =
        match tokio::time::timeout(std::time::Duration::from_secs(SHELL_TIMEOUT_SECS), fut).await {
            Err(_) => return format!("[timed out after {SHELL_TIMEOUT_SECS}s]"),
            Ok(Err(e)) => return format!("[failed to run: {e}]"),
            Ok(Ok(out)) => out,
        };
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&err);
    }
    if !out.status.success() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!("[exit {}]", out.status.code().unwrap_or(-1)));
    }
    if text.len() > SHELL_MAX_BYTES {
        let mut cut = SHELL_MAX_BYTES;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str("\n[output truncated]");
    }
    text.trim_end().to_string()
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
    pub prompt: String,
    /// When the CLI first saw this approval request; used to show the
    /// auto-deny countdown against the gate's DEFAULT_TIMEOUT.
    pub created_at: std::time::Instant,
}

impl HitlRequest {
    fn from_value(v: Value) -> Self {
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
            prompt: v
                .get("prompt")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("Run `{tool_name}`?")),
            tool_input: v.get("tool_input").cloned().unwrap_or(Value::Null),
            tool_name,
            created_at: std::time::Instant::now(),
        }
    }
}

/// Build the `message/send` params for one turn, threading the previous turn's
/// task id as `context.task_id` so the agent keeps conversation history.
/// `image`, when set, is `(mime, base64)` attached as a second A2A `data` part
/// (an inline screenshot or pasted image file) that the runtime forwards to the
/// model as a vision input.
pub fn build_params(
    text: &str,
    task_id: &str,
    context_task_id: Option<&str>,
    image: Option<(&str, &str)>,
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
        params["context"] = json!({ "task_id": tid });
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
                let _ = tx.blocking_send(StreamMsg::Hitl {
                    task_id: tid.clone(),
                    req: HitlRequest::from_value(hitl),
                });
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
                    } => StreamMsg::StepCompleted {
                        task_id,
                        step_id,
                        ok,
                        output,
                        truncated,
                        full_len,
                        error,
                        duration_ms,
                    },
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

/// Answer a pending HITL request on a fresh connection. Does not block the
/// streaming worker; the agent resumes once the runtime receives this.
pub async fn respond_hitl(
    home: PathBuf,
    agent: String,
    hitl_id: String,
    allow: bool,
) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        dial_method(
            &home,
            &agent,
            "tool/hitl_respond",
            json!({ "hitl_id": hitl_id, "allow": allow }),
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
mod tests {
    use super::*;

    #[test]
    fn build_params_threads_context() {
        let p = build_params("hi", "t-2", Some("t-1"), None);
        assert_eq!(p["message"]["parts"][0]["text"], "hi");
        assert_eq!(p["task_id"], "t-2");
        assert_eq!(p["context"]["task_id"], "t-1");
    }

    #[test]
    fn build_params_first_turn_has_no_context() {
        let p = build_params("hi", "t-1", None, None);
        assert!(p.get("context").is_none());
    }

    #[test]
    fn build_params_attaches_image_as_data_part() {
        let p = build_params(
            "what is this?",
            "t-3",
            None,
            Some(("image/png", "QkFTRTY0")),
        );
        let parts = p["message"]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2, "text part + image data part");
        assert_eq!(parts[0]["kind"], "text");
        assert_eq!(parts[1]["kind"], "data");
        assert_eq!(parts[1]["mimeType"], "image/png");
        assert_eq!(parts[1]["data"]["base64"], "QkFTRTY0");
        // A non-PNG paste threads its own mime.
        let jpg = build_params("x", "t-5", None, Some(("image/jpeg", "QQ==")));
        assert_eq!(jpg["message"]["parts"][1]["mimeType"], "image/jpeg");
        // No image → no data part (back-compat).
        let plain = build_params("hi", "t-4", None, None);
        assert_eq!(plain["message"]["parts"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn extract_reply_finds_last_agent_message() {
        let task = json!({
            "id": "task-9",
            "messages": [
                { "role": "user", "parts": [{ "kind": "text", "text": "q" }] },
                { "role": "agent", "parts": [
                    { "kind": "text", "text": "Hello " },
                    { "kind": "text", "text": "world" }
                ]}
            ]
        });
        let (reply, id) = extract_reply(&task);
        assert_eq!(reply, "Hello world");
        assert_eq!(id.as_deref(), Some("task-9"));
    }

    #[test]
    fn task_outcome_failed_surfaces_error() {
        let task = json!({ "id": "t", "state": "failed", "error": { "message": "rate limited" }, "messages": [] });
        assert_eq!(task_outcome(&task).unwrap_err(), "rate limited");
    }

    #[test]
    fn task_outcome_cancelled_without_error_message() {
        let task = json!({ "id": "t", "state": "cancelled", "messages": [] });
        assert_eq!(task_outcome(&task).unwrap_err(), "agent task cancelled");
    }

    #[test]
    fn task_outcome_completed_returns_reply() {
        let task = json!({
            "id": "t9",
            "state": "completed",
            "messages": [{ "role": "agent", "parts": [{ "kind": "text", "text": "hi" }] }]
        });
        let (reply, id) = task_outcome(&task).unwrap();
        assert_eq!(reply, "hi");
        assert_eq!(id.as_deref(), Some("t9"));
    }

    // sh-syntax test commands — unix only; Windows goes through `cmd /C`.
    #[cfg(unix)]
    #[tokio::test]
    async fn run_local_shell_captures_output_and_exit() {
        let out = run_local_shell("echo hi; echo err >&2; exit 3".into()).await;
        assert!(out.contains("hi"));
        assert!(out.contains("err"));
        assert!(out.contains("[exit 3]"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_local_shell_truncates_huge_output() {
        let out = run_local_shell("yes x | head -c 100000".into()).await;
        assert!(out.len() <= SHELL_MAX_BYTES + 64);
        assert!(out.ends_with("[output truncated]"));
    }

    #[test]
    fn stream_msg_task_id_accessor() {
        let d = StreamMsg::Delta {
            task_id: "x".into(),
            text: "a".into(),
            thinking: false,
        };
        assert_eq!(d.task_id(), Some("x"));
        assert_eq!(StreamMsg::Note("n".into()).task_id(), None);
    }

    #[test]
    fn hitl_request_parses_fields_with_fallback() {
        let h = HitlRequest::from_value(json!({
            "hitl_id": "h1",
            "tool_name": "bash",
            "tool_input": { "command": "ls" }
        }));
        assert_eq!(h.hitl_id, "h1");
        assert_eq!(h.tool_name, "bash");
        assert_eq!(h.prompt, "Run `bash`?");
        assert_eq!(h.tool_input["command"], "ls");
    }
}

#[cfg(test)]
mod hitl_step_tests {
    use super::HitlRequest;

    #[test]
    fn parses_step_id_when_present() {
        let v = serde_json::json!({
            "step_id": "s-1", "hitl_id": "h-1", "tool_name": "edit",
            "tool_input": { "file_path": "a.rs" }, "prompt": "Run `edit`?"
        });
        let req = HitlRequest::from_value(v);
        assert_eq!(req.step_id.as_deref(), Some("s-1"));
        assert_eq!(req.hitl_id, "h-1");
    }

    #[test]
    fn step_id_none_on_old_runtime() {
        let v = serde_json::json!({ "hitl_id": "h-1", "tool_name": "bash" });
        let req = HitlRequest::from_value(v);
        assert!(req.step_id.is_none());
    }
}
