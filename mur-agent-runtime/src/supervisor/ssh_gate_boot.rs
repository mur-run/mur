//! Fill the ssh-agent proxy's approval gate (plan Task 3.2) once the
//! supervisor's HITL channel exists. Split from `mod.rs` so the boot
//! sequence stays readable; nothing here runs before the seal.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use mur_common::agent::AgentProfile;
use mur_common::identity::AgentIdentity;

use super::seal::SshGateSlot;
use crate::hitl::HitlApprovals;
use crate::hitl::parked::Parking;
use crate::hitl::ssh_gate::SshSignGate;
use crate::hitl::store::ChannelDecisionStore;

/// Gate B's parts, as the supervisor already holds them.
pub(super) struct Parts<'a> {
    pub mur_home: &'a Path,
    pub profile: &'a AgentProfile,
    pub identity: &'a Arc<AgentIdentity>,
    pub approvals: HitlApprovals,
    pub notifier: tokio::sync::mpsc::Sender<serde_json::Value>,
    pub parking: Parking,
    /// `hitl.timeout_secs`: how long one sign request waits before deferring.
    pub wait_secs: u32,
}

pub(super) fn fill(g: SshGateSlot, p: Parts<'_>) {
    // Same store as the chat gate, so `mur agent hitl list|forget` sees ssh
    // decisions with no ssh-specific cache (spec "HITL wiring").
    let store = Arc::new(ChannelDecisionStore::new(
        p.mur_home.to_path_buf(),
        p.profile.name.clone(),
        p.identity.clone(),
        p.profile.identity.key_version,
    ));
    let gate = SshSignGate {
        agent: p.profile.name.clone(),
        store,
        approvals: p.approvals,
        notifier: p.notifier,
        parking: p.parking,
        wait: Duration::from_secs(u64::from(p.wait_secs)),
        audit_only: g.audit_only,
        pending: Default::default(),
    };
    if !g.slot.set(Arc::new(gate)) {
        tracing::warn!("ssh-agent proxy approval gate was already set; keeping the first");
    }
}
