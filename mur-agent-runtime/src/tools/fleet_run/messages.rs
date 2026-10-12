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
