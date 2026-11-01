//! `mur agent ssh ...` — which ssh keys an agent may sign with, per host.

use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum AgentSshAction {
    /// Let the agent request signatures with one of your ssh-agent keys for
    /// one host. Every signature still goes through HITL.
    ///
    /// With --host the host key is scanned once and shown for confirmation,
    /// and a known_hosts entry is written for the agent's ssh. Labels are
    /// display only: the proxy matches fingerprints and nothing else.
    Allow {
        /// Agent name
        agent: String,
        /// Key fingerprint (SHA256:..., from `ssh-add -l`), or a key comment
        /// looked up in your ssh-agent and confirmed (the comment is not stored)
        #[arg(long)]
        key: String,
        /// Display label for the key (not used for matching)
        #[arg(long)]
        label: Option<String>,
        /// Hostname to scan for its host key (needs a terminal to confirm)
        #[arg(long, conflicts_with = "yes")]
        host: Option<String>,
        /// Port to scan with --host (default 22)
        #[arg(long, requires = "host")]
        port: Option<u16>,
        /// Host key fingerprint you verified out of band. With --host, the
        /// scanned key must match it; alone, no known_hosts entry is written
        #[arg(long)]
        host_key: Option<String>,
        /// Display label for the host (not used for matching; defaults to --host)
        #[arg(long)]
        host_label: Option<String>,
        /// Skip the confirmation. Needs --key and --host-key as fingerprints;
        /// never scans and never adds the ssh spawn grant
        #[arg(long, requires = "host_key")]
        yes: bool,
    },
    /// Take a grant back. --key and --host-key together revoke one pair;
    /// either alone revokes every pair it names (confirmed first)
    Revoke {
        /// Agent name
        agent: String,
        /// Key fingerprint
        #[arg(long, required_unless_present = "host_key")]
        key: Option<String>,
        /// Host key fingerprint
        #[arg(long)]
        host_key: Option<String>,
        /// Confirm a batch revoke without a prompt
        #[arg(long)]
        yes: bool,
    },
    /// Show the keys and host keys the agent may sign for
    List {
        /// Agent name
        agent: String,
        /// Print the `entitlements.ssh` block as JSON
        #[arg(long)]
        json: bool,
    },
}
