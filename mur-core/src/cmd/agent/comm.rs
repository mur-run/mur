//! A2A forwarders — `mur agent send` and `mur agent card`.

use anyhow::{Context, Result};
use mur_common::config::Config;

use crate::a2a_dial::{DialMode, dial_method};

use super::resolve_mur_home;

pub fn cmd_send(
    name: &str,
    message_json: &str,
    output_artifact_path: Option<&str>,
    wait_approval: Option<&str>,
) -> Result<()> {
    let msg: serde_json::Value =
        serde_json::from_str(message_json).context("parse --message JSON")?;
    // Parsed before anything is sent: a typo must not cost a turn.
    let wait = wait_approval.map(parse_wait).transpose()?;
    let home = resolve_mur_home()?;
    // Required-budget gate (plan §9), same refusal the TUI and --plain apply.
    // A script is still a turn: the runtime injects every Required memory
    // unconditionally, so sending while over the reservation would inject a
    // prompt the interactive user is blocked from sending.
    if let Some(overlay) = crate::cmd::agent::cli::memory_cmds::required_budget_block(&home, name) {
        anyhow::bail!("{overlay}");
    }
    // One-shot send: the caller is a script or a shell, not a person watching
    // for a prompt. Saying so lets the runtime refuse a gated tool at once
    // instead of holding the turn open for `hitl.timeout_secs` waiting on an
    // approval nobody is there to give.
    let mut params = serde_json::json!({"message": msg, "can_approve": false});
    if let Some(path) = output_artifact_path {
        params["output_artifact_path"] = serde_json::json!(path);
    }
    let result = match wait {
        None => {
            // `message/send` to an ephemeral runtime is meaningless — the task
            // would die with the process. Require the agent be running.
            dial_method(
                &home,
                name,
                "message/send",
                params,
                DialMode::RequireRunning,
            )?
        }
        Some(wait) => send_waiting(&home, name, params, wait)?,
    };
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

/// #2085: `--wait-approval[=DURATION]`. Empty = the default wait.
fn parse_wait(raw: &str) -> Result<std::time::Duration> {
    if raw.trim().is_empty() {
        return Ok(std::time::Duration::from_secs(
            mur_common::hitl::DEFAULT_WAIT_APPROVAL_SECS,
        ));
    }
    match mur_common::limits::parse_duration(raw) {
        Some(d) if !d.is_zero() => Ok(d),
        _ => anyhow::bail!(
            "--wait-approval: '{raw}' is not a duration; use e.g. 30s, 10m, 1h (or omit the value for 30m)"
        ),
    }
}

/// Send and stay on the line while a parked approval waits. Streaming, not a
/// plain dial, for two reasons: the approval request arrives as a
/// notification, which is where the approve command gets printed (stderr, so
/// stdout stays the one JSON result); and the runtime's heartbeat keeps the
/// dial's idle timeout from firing while nobody has answered yet.
fn send_waiting(
    home: &std::path::Path,
    name: &str,
    mut params: serde_json::Value,
    wait: std::time::Duration,
) -> Result<serde_json::Value> {
    let agent = refuse_old_runtime(home, name)?;
    params[mur_common::hitl::WAIT_APPROVAL_PARAM] = serde_json::json!(wait.as_secs());
    crate::a2a_dial::dial_message_streaming(
        home,
        &agent,
        params,
        |_, _, _| {},
        |hitl| {
            for line in super::hitl_pending::approve_hint(&agent, &hitl) {
                eprintln!("{line}");
            }
        },
        |_| {},
    )
}

/// Resolve `typed` the way the dial will (agent names are case-insensitive)
/// and refuse up front if that agent's runtime predates parked approvals —
/// an older runtime ignores the wait param and refuses at once, so the flag
/// would silently do nothing. Returns the canonical name to dial.
fn refuse_old_runtime(home: &std::path::Path, typed: &str) -> Result<String> {
    let agent = crate::a2a_dial::canonicalize_agent_name(home, typed);
    let lock = home.join("agents").join(&agent).join("running.lock");
    if let Ok(bytes) = std::fs::read(&lock)
        && let Ok(l) = serde_json::from_slice::<mur_common::LockFile>(&bytes)
        && l.proto_version < mur_common::build::HITL_PARK_MIN_PROTO
    {
        anyhow::bail!(
            "agent '{agent}' is running a runtime too old to wait for approvals (proto {}); \
             run 'mur agent restart {agent}' after 'mur update', or drop --wait-approval",
            l.proto_version
        );
    }
    Ok(agent)
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

#[cfg(test)]
mod wait_tests {
    use super::parse_wait;
    use std::time::Duration;

    #[test]
    fn a_bare_flag_waits_the_default() {
        assert_eq!(
            parse_wait("").unwrap(),
            Duration::from_secs(mur_common::hitl::DEFAULT_WAIT_APPROVAL_SECS)
        );
    }

    #[test]
    fn a_duration_is_honoured() {
        assert_eq!(parse_wait("10m").unwrap(), Duration::from_secs(600));
    }

    #[test]
    fn zero_and_nonsense_are_refused_before_sending() {
        assert!(parse_wait("0").is_err());
        assert!(parse_wait("soon").is_err());
    }
}

#[cfg(test)]
mod old_runtime_tests {
    use super::refuse_old_runtime;

    fn home_with_agent(dir: &str, proto: u32) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let agent = home.path().join("agents").join(dir);
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("profile.yaml"), "name: mur\n").unwrap();
        let lock = serde_json::json!({
            "schema": 1, "uuid": "u", "name": dir, "pid": 1, "ppid": 1,
            "started_at": "2026-01-01T00:00:00Z", "binary_version": "x",
            "transports": {"stdio": false, "unix_socket": null, "tcp": null, "webhook": null},
            "card_digest": "", "capabilities": [], "proto_version": proto,
        });
        std::fs::write(agent.join("running.lock"), lock.to_string()).unwrap();
        home
    }

    #[test]
    fn mixed_case_name_still_hits_the_old_runtime_check() {
        let home = home_with_agent("mur", mur_common::build::HITL_PARK_MIN_PROTO - 1);
        let e = refuse_old_runtime(home.path(), "Mur")
            .unwrap_err()
            .to_string();
        assert!(e.contains("too old"), "{e}");
    }

    #[test]
    fn a_current_runtime_passes() {
        let home = home_with_agent("mur", mur_common::build::HITL_PARK_MIN_PROTO);
        refuse_old_runtime(home.path(), "Mur").unwrap();
    }
}
