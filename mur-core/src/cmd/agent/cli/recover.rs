//! Recovery policy for a murmur turn when the agent runtime restarts
//! mid-task. The runtime keeps tasks in memory only, so a restart orphans
//! every task the TUI is bound to: steering it fails with a JSON-RPC
//! "task not found" (#713), and a `message/send` dial that raced the restart
//! can die after the user's message was already persisted to the channel
//! (#714). These pure helpers decide what the event loop should do next;
//! the wiring stays in `mod.rs`.

use serde_json::Value;

/// What to do after a `turn/steer` dial fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteerFailure {
    /// The runtime no longer knows the task — it restarted (in-memory tasks
    /// are gone) or is not running at all. The dead binding must be dropped
    /// and the user's text resent as a fresh `message/send` on the same
    /// channel so it is not lost.
    TaskGone,
    /// Any other failure: surface it, keep the turn bound.
    Other,
}

/// What to do after a `tool/hitl_respond` dial fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitlFailure {
    /// The gate already auto-denied at timeout ("approval expired" on new
    /// runtimes, "task not found" on older ones). The turn itself may still
    /// be alive on the runtime, so only the approval is stale.
    Expired,
    /// The runtime is gone (restarted or stopped): the in-flight task no
    /// longer exists anywhere, so the TUI must also drop its task binding or
    /// every subsequent input steers a corpse.
    AgentGone,
    /// Any other failure: surface it, change nothing.
    Other,
}

/// Extract the `message` of a JSON-RPC error object embedded in a dial error
/// display string (`agent 'x' returned error: {"code":-32600,"message":…}`).
/// Tolerates trailing context after the object. `None` when no parseable
/// object is present (e.g. a transport-level failure).
fn jsonrpc_error_message(err: &str) -> Option<String> {
    let start = err.find('{')?;
    let mut stream = serde_json::Deserializer::from_str(&err[start..]).into_iter::<Value>();
    let v = stream.next()?.ok()?;
    v.get("message")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// True when a dial error means the target agent has no `running.lock`
/// (`a2a_dial` phrases this as "agent 'x' is not running (no …)").
fn is_agent_down(err: &str) -> bool {
    err.contains("is not running")
}

/// Classify a `turn/steer` failure. Matches the JSON-RPC error message when
/// one is embedded, falling back to the raw display string, so a runtime that
/// serializes its error differently still classifies correctly.
pub fn classify_steer_failure(err: &str) -> SteerFailure {
    if is_agent_down(err) {
        return SteerFailure::TaskGone;
    }
    let msg = jsonrpc_error_message(err).unwrap_or_else(|| err.to_string());
    if msg.to_ascii_lowercase().contains("task not found") {
        SteerFailure::TaskGone
    } else {
        SteerFailure::Other
    }
}

/// Classify a `tool/hitl_respond` failure.
pub fn classify_hitl_failure(err: &str) -> HitlFailure {
    if is_agent_down(err) {
        return HitlFailure::AgentGone;
    }
    let msg = jsonrpc_error_message(err)
        .unwrap_or_else(|| err.to_string())
        .to_ascii_lowercase();
    if msg.contains("approval expired") || msg.contains("task not found") {
        HitlFailure::Expired
    } else {
        HitlFailure::Other
    }
}

/// After the streaming `message/send` dial reports an error: retry the dial
/// once if the runtime never produced anything for this turn (no delta, step,
/// or HITL event) — the user's message is already persisted in the channel,
/// so a turn that failed to *start* must not be dropped silently (#714). A
/// turn that already produced output is not replayed (the agent may have done
/// side-effectful work), and only one retry is attempted.
pub fn should_retry_send(turn_produced_output: bool, already_retried: bool) -> bool {
    !turn_produced_output && !already_retried
}

#[cfg(test)]
mod tests {
    use super::*;

    // The exact shape observed in #713: dial_method interpolates the JSON-RPC
    // error object into the display string.
    const STEER_TASK_NOT_FOUND: &str = r#"agent 'mur' returned error: {"code":-32600,"message":"task not found: 019169fb-aaaa-bbbb-cccc-000000000000"}"#;
    const AGENT_DOWN: &str =
        "agent 'mur' is not running (no /Users/x/.mur/agents/mur/running.lock)";

    #[test]
    fn steer_task_not_found_json_rpc_is_task_gone() {
        assert_eq!(
            classify_steer_failure(STEER_TASK_NOT_FOUND),
            SteerFailure::TaskGone
        );
    }

    #[test]
    fn steer_agent_down_is_task_gone() {
        assert_eq!(classify_steer_failure(AGENT_DOWN), SteerFailure::TaskGone);
    }

    #[test]
    fn steer_raw_task_not_found_without_json_is_task_gone() {
        assert_eq!(
            classify_steer_failure("Task Not Found: abc"),
            SteerFailure::TaskGone
        );
    }

    #[test]
    fn steer_unrelated_error_is_other() {
        assert_eq!(
            classify_steer_failure("connect /tmp/a2a.sock: connection refused"),
            SteerFailure::Other
        );
        // A JSON-RPC error with a different message must not match.
        assert_eq!(
            classify_steer_failure(
                r#"agent 'mur' returned error: {"code":-32603,"message":"internal error"}"#
            ),
            SteerFailure::Other
        );
    }

    #[test]
    fn steer_json_with_trailing_context_still_parses() {
        let err = r#"agent 'mur' returned error: {"code":-32600,"message":"task not found: x"}: dial context"#;
        assert_eq!(classify_steer_failure(err), SteerFailure::TaskGone);
    }

    #[test]
    fn hitl_agent_down_is_agent_gone() {
        assert_eq!(classify_hitl_failure(AGENT_DOWN), HitlFailure::AgentGone);
    }

    #[test]
    fn hitl_expired_variants() {
        assert_eq!(
            classify_hitl_failure(
                r#"agent 'mur' returned error: {"code":-32600,"message":"approval expired"}"#
            ),
            HitlFailure::Expired
        );
        assert_eq!(
            classify_hitl_failure("task not found: 0191"),
            HitlFailure::Expired
        );
    }

    #[test]
    fn hitl_unrelated_error_is_other() {
        assert_eq!(
            classify_hitl_failure("write request: broken pipe"),
            HitlFailure::Other
        );
    }

    #[test]
    fn retry_send_only_once_and_only_before_output() {
        assert!(should_retry_send(false, false));
        assert!(!should_retry_send(true, false)); // turn already started
        assert!(!should_retry_send(false, true)); // already retried
        assert!(!should_retry_send(true, true));
    }
}
