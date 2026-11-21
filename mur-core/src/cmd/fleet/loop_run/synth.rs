use super::*;

pub(super) fn build_synthesis_prompt(goal: &str, marker: &str, evidence: &str) -> String {
    format!(
        "You are the final synthesizer. Goal: {goal}\n\nWorker evidence:\n{evidence}\n\nWrite the final cited answer now. Do not delegate or propose more work. If the evidence is insufficient, state the limitations explicitly but still finish with the convergence marker as the last non-blank line, alone exactly as:\n{marker}\n"
    )
}

pub(super) fn finalize_synthesis(reply: &str, marker: &str) -> String {
    let mut lines: Vec<&str> = reply.lines().collect();
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    if lines.last().is_some_and(|line| line.trim() == marker) {
        return lines.join("\n");
    }
    let body = lines.join("\n");
    if body.is_empty() {
        marker.to_string()
    } else {
        format!("{body}\n\n{marker}")
    }
}

pub(crate) fn extract_task_reply(task: &serde_json::Value) -> String {
    task.get("messages")
        .and_then(|m| m.as_array())
        .and_then(|messages| {
            messages
                .iter()
                .rev()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("agent"))
        })
        .and_then(|m| m.get("parts").and_then(|p| p.as_array()))
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

pub(super) fn task_tokens(task: &serde_json::Value) -> u64 {
    let Some(usage) = task.get("usage") else {
        return 0;
    };
    usage
        .get("input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        .saturating_add(
            usage
                .get("output_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        )
}

/// Run the router's synthesis turn and record its reply on the fleet channel.
///
/// Returns the tokens the turn used. `Err` means the turn itself did not run
/// to a reply; a reply that could not be recorded is logged and still counted.
pub(super) fn synthesize_via_router(
    mur_home: &Path,
    fleet: &mur_common::fleet::Fleet,
    goal: &str,
    marker: &str,
    evidence: &str,
) -> Result<u64> {
    let prompt = build_synthesis_prompt(goal, marker, evidence);
    let params = serde_json::json!({
        "message": { "role": "user", "parts": [{ "kind": "text", "text": prompt }] },
        "context": { "channel_id": fleet.channel_id }
    });
    let mut streamed = String::new();
    let task = crate::a2a_dial::dial_message_streaming(
        mur_home,
        fleet.router_or_concierge(),
        params,
        |delta, thinking, _id| {
            if !thinking {
                streamed.push_str(delta);
            }
        },
        |_hitl| {},
        |_step| {},
    )?;
    let reply = finalize_synthesis(
        &{
            let final_reply = extract_task_reply(&task);
            if final_reply.trim().is_empty() {
                streamed
            } else {
                final_reply
            }
        },
        marker,
    );
    // The turn is paid for once it returns: a synthesis that cannot be
    // written is a lost line, not a free turn (#2520).
    let tokens = task_tokens(&task);
    if let Err(error) = record_synthesis(mur_home, fleet, &reply) {
        tracing::warn!(
            fleet = %fleet.name,
            channel_id = %fleet.channel_id,
            tokens,
            %error,
            "fleet loop: the synthesis could not be written; its turn is still billed"
        );
    }
    Ok(tokens)
}

/// Write the router's synthesis to the fleet channel, as the router.
///
/// The line is the router's own (`Agent`, its key), but the head stays the
/// control plane's: with `router: orchestrator` the two are different keys,
/// and a head under the router's would strand every `System` write after it.
pub(super) fn record_synthesis(
    mur_home: &Path,
    fleet: &mur_common::fleet::Fleet,
    reply: &str,
) -> Result<ChannelEvent> {
    let svc = mur_channel::ChannelService::open(mur_home)?;
    crate::channel_writer::append_agent_line_router_sealed(
        &svc,
        mur_home,
        &fleet.channel_id,
        fleet.router_or_concierge(),
        mur_common::channel::EventKind::Message,
        serde_json::json!({ "text": reply }),
        None,
    )
}

/// Ask the router agent whether the goal is complete. Streams a one-word reply.
pub(super) fn ask_router_done(
    mur_home: &Path,
    fleet: &Fleet,
    events: &[ChannelEvent],
) -> Result<bool> {
    let recent: String = events
        .iter()
        .rev()
        .take(8)
        .rev()
        .filter_map(|e| e.payload.get("text").and_then(|v| v.as_str()))
        .map(|t| format!("- {t}"))
        .collect::<Vec<_>>()
        .join("\n");
    let done_when = fleet
        .loop_cfg
        .as_ref()
        .map(|l| l.done_when.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("(none given — judge from the goal)");
    let prompt = format!(
        "You are the router for fleet '{}'.\nGoal: {}\nDone-criterion: {}\nRecent channel activity:\n{}\n\nIs the goal complete? Reply with exactly one word: DONE or CONTINUE.",
        fleet.name,
        fleet.goal,
        done_when,
        if recent.is_empty() {
            "(none yet)"
        } else {
            &recent
        },
    );
    let params = serde_json::json!({
        "message": { "role": "user", "parts": [{ "kind": "text", "text": prompt }] }
    });
    let mut out = String::new();
    crate::a2a_dial::dial_message_streaming(
        mur_home,
        fleet.router_or_concierge(),
        params,
        |delta, _thinking, _id| out.push_str(delta),
        |_hitl| {},
        |_step| {},
    )?;
    Ok(is_converged(&out))
}
