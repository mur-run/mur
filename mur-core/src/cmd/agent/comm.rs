//! A2A forwarders — `mur agent send` and `mur agent card`.

use anyhow::{Context, Result};
use mur_common::config::Config;

use crate::a2a_dial::{DialMode, dial_method};

use super::resolve_mur_home;

pub fn cmd_send(name: &str, message_json: &str, output_artifact_path: Option<&str>) -> Result<()> {
    let msg: serde_json::Value =
        serde_json::from_str(message_json).context("parse --message JSON")?;
    let home = resolve_mur_home()?;
    // One-shot send: the caller is a script or a shell, not a person watching
    // for a prompt. Saying so lets the runtime refuse a gated tool at once
    // instead of holding the turn open for `hitl.timeout_secs` waiting on an
    // approval nobody is there to give.
    let mut params = serde_json::json!({"message": msg, "can_approve": false});
    if let Some(path) = output_artifact_path {
        params["output_artifact_path"] = serde_json::json!(path);
    }
    // `message/send` to an ephemeral runtime is meaningless — the task
    // would die with the process. Require the agent be running.
    let result = dial_method(
        &home,
        name,
        "message/send",
        params,
        DialMode::RequireRunning,
    )?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

/// Translate a raw JSON-RPC failure into something the operator can act on.
///
/// The dial helper flattens a JSON-RPC error into its message, so this matches
/// on the code in the text — the only seam available without reshaping
/// `dial_method`'s return type for one caller. `-32601` is "method not found",
/// which here almost always means the agent is running a runtime older than the
/// method. The proto-version gate inside `dial_method` catches the versioned
/// methods before they reach the socket; this covers everything else.
fn explain_dial_error(name: &str, method: &str, e: anyhow::Error) -> anyhow::Error {
    if e.to_string().contains("-32601") {
        anyhow::anyhow!(
            "agent '{name}' does not serve '{method}' — either the method name is wrong, \
             or this agent's running runtime predates it. Check the spelling first; \
             'mur agent restart {name}' (after 'mur update') rules out the second."
        )
    } else {
        e
    }
}

/// `mur agent dial <name> <method> [params-json]` — call any A2A method on a
/// running agent and print its raw `result`.
///
/// Deliberately a passthrough rather than an allowlist. The unix socket is the
/// trust boundary and its owner can already hand-roll the same JSON-RPC frame;
/// a list of permitted methods here would be a second source of truth that goes
/// stale the moment the runtime registers another one. What a method will
/// answer is the handler's decision, not this command's.
pub fn cmd_dial(name: &str, method: &str, params: Option<&str>) -> Result<()> {
    let home = resolve_mur_home()?;
    let params: serde_json::Value = match params {
        None => serde_json::json!({}),
        Some(raw) => {
            serde_json::from_str(raw).with_context(|| format!("parse params as JSON: {raw}"))?
        }
    };

    // `RequireRunning`, not `Auto`: dialing a stopped agent would spawn an
    // ephemeral runtime, and a reload or a cancel aimed at a process that exits
    // moments later is a no-op the caller would read as success.
    let result = dial_method(&home, name, method, params, DialMode::RequireRunning)
        .map_err(|e| explain_dial_error(name, method, e))?;

    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

pub fn cmd_card(name: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    let mut result = dial_method(
        &home,
        name,
        "agent/card",
        serde_json::Value::Null,
        DialMode::Auto,
    )?;

    // Fitness section (M7a) — embedded into the card JSON so stdout stays valid JSON
    let cfg = Config::load_or_default(&home.join("config.yaml"));
    let fitness = crate::cross_agent::fitness::fitness(
        &home,
        name,
        chrono::Utc::now(),
        cfg.cross_agent.fitness_half_life_days,
        cfg.cross_agent.fitness_floor,
    )?;
    let fitness_json = serde_json::json!({
        "weight": fitness.weight,
        "success_rate": fitness.success_rate,
        "sample_size": fitness.sample_size,
        "recency_decay": fitness.recency_decay,
        "last_seen": fitness.last_seen.map(|t| t.to_rfc3339()),
    });
    if let Some(obj) = result.as_object_mut() {
        obj.insert("fitness".into(), fitness_json);
    }

    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

#[cfg(test)]
mod dial_tests {
    use super::explain_dial_error;

    #[test]
    fn method_not_found_becomes_a_stale_runtime_message() {
        let raw = anyhow::anyhow!(
            r#"agent 'mur' returned error: {{"code":-32601,"message":"method not found"}}"#
        );
        let out = explain_dial_error("mur", "memory/reload", raw).to_string();
        assert!(out.contains("predates it"), "{out}");
        assert!(out.contains("mur agent restart mur"), "{out}");
        assert!(
            !out.contains("-32601"),
            "the raw code must not survive: {out}"
        );
        // Must not assert a cause it cannot distinguish: a mistyped method and
        // a stale runtime produce the identical -32601.
        assert!(
            out.contains("method name is wrong"),
            "the typo case must be named too: {out}"
        );
    }

    /// Negative control: every other failure passes through untouched. A dial
    /// command that rewrites errors it does not understand is worse than one
    /// that reports them verbatim.
    #[test]
    fn other_errors_are_passed_through_verbatim() {
        let raw = anyhow::anyhow!("agent 'mur' is not running (no /x/running.lock)");
        let out = explain_dial_error("mur", "tasks/list", raw).to_string();
        assert_eq!(out, "agent 'mur' is not running (no /x/running.lock)");
    }
}
