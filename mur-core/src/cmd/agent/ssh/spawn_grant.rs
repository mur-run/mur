//! Plan O3: `ssh` itself must be spawnable for any of this to be used.
//!
//! The grant is written by `setup::apply_and_restart` together with the SSH
//! grant; this module only names the binary, the rule and the manual command.

/// The binary and the rule live in `mur-ssh-request` so the runtime's
/// `ssh_request` tool answers with the same verdict.
pub use mur_ssh_request::{SSH_BINARY as SSH_SPAWN_BINARY, ssh_spawnable};

/// The manual command, printed whenever the grant is not added here.
pub fn manual_command(agent: &str) -> String {
    format!("mur agent perm allow-spawn {agent} {SSH_SPAWN_BINARY}")
}
