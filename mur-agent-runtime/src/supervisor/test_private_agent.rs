//! Test-only entry to the boot-time private ssh-agent start (plan Task 16.8),
//! for integration tests that drive a real `ssh-agent` against the proxy.
//! Same shape as [`super::test_bridge`]: the production path, exported only
//! as a narrow wrapper so the boot internals stay `pub(super)`.

use std::path::Path;
use std::sync::Arc;

use mur_common::agent::SshEntitlement;

use crate::ssh_proxy::listener::DedicatedUpstream;

/// Run the runtime's pre-seal start for `agent` under `mur_home` exactly as
/// boot does, including the clean-up of the key from the inherited
/// `SSH_AUTH_SOCK`. `None` when nothing was started (see the boot log).
pub async fn start_private_agent_for_test(
    mur_home: &Path,
    agent: &str,
    ent: &SshEntitlement,
) -> Option<Arc<DedicatedUpstream>> {
    super::ssh_proxy_boot::start_private_before_seal(mur_home, agent, ent)
        .await
        .upstream
}
