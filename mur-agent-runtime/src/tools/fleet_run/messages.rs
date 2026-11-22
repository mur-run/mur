//! Operator-facing refusal and spawn-failure text.

/// Refusal for an unreadable `config.yaml`: still deny (fail-closed), but name
/// the read error and the restart, not an allowlist edit (#1795).
pub(super) fn unreadable_config_message(err: &str) -> String {
    mur_common::authz::not_authorized(&format!(
        "fleet_run: could not re-read the fleet_run allowlist ({err}); refusing until it \
         can be read. If ~/.mur/config.yaml was edited since this agent started, \
         restart the agent to pick up the change"
    ))
}

/// Explain a failed spawn without sending the reader down a wrong path.
///
/// Only a permission denial can be the sandbox, and the usual cause is the
/// binary living outside every exec lane (e.g. under `$TMPDIR`) — which no
/// restart fixes. A restart only helps when `fleet_run.agents` changed after
/// this agent started, so it is offered on that condition alone.
pub(crate) fn spawn_error_message(bin: &std::path::Path, e: &std::io::Error) -> String {
    let base = format!("failed to spawn `{}`: {e}", bin.display());
    if e.kind() != std::io::ErrorKind::PermissionDenied {
        return base;
    }
    format!(
        "{base} — the runtime sandbox refused to exec this path; check that it \
         is inside an exec lane (binaries under $TMPDIR or /tmp never are). \
         Restarting the agent helps only if `fleet_run.agents` in config.yaml \
         changed after it started"
    )
}

/// Why this run cannot be spawned here, as the opening clause of a refusal.
///
/// Two different keys can be the missing one. A fleet routed by another agent
/// needs that router's key. A fleet this agent routes itself needs the control
/// plane's, for the run's `System` events — and telling an agent it "cannot
/// sign as" its own name would be false.
pub(super) fn needs_the_daemon_because(fleet: &str, agent: &str, router: &str) -> String {
    if router.eq_ignore_ascii_case(agent) {
        format!(
            "fleet '{fleet}' is routed by this agent, but the run's control events (pause, \
             stop, approval requests) are signed by the control plane ('{}'), whose key this \
             agent does not hold",
            mur_channel::verify::CONTROL_PLANE_AGENT
        )
    } else {
        format!(
            "fleet '{fleet}' is routed by '{router}' and its events are signed as '{router}', \
             which this agent cannot do"
        )
    }
}
