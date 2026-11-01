//! `mur agent ssh ...` → `cmd::agent::cmd_ssh_*`.

use crate::cmd;

pub(super) fn dispatch(action: crate::cli::AgentSshAction) -> anyhow::Result<()> {
    use crate::cli::AgentSshAction as A;
    match action {
        A::Allow {
            agent,
            key,
            label,
            host,
            port,
            host_key,
            host_label,
            yes,
        } => cmd::agent::cmd_ssh_allow(&cmd::agent::SshAllowArgs {
            agent: &agent,
            key: &key,
            label: label.as_deref(),
            host: host.as_deref(),
            port,
            host_key: host_key.as_deref(),
            host_label: host_label.as_deref(),
            yes,
        }),
        A::Revoke {
            agent,
            key,
            host_key,
            yes,
        } => cmd::agent::cmd_ssh_revoke(&cmd::agent::SshRevokeArgs {
            agent: &agent,
            key: key.as_deref(),
            host_key: host_key.as_deref(),
            yes,
        }),
        A::List { agent, json } => cmd::agent::cmd_ssh_list(&agent, json),
    }
}
