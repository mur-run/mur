//! Refuse to start a fleet run that could not sign its own events (#2531).
//!
//! `mur fleet run` from inside an agent's sandbox skips every `fleet_run`
//! gate, and the process holds no key: `keys/` is denied there and nothing
//! handed one over. Such a run would write `run_paused`, `run_resumed`, the
//! stop `StateChange` and its router's lines unsigned, and could never park
//! an approval. The `fleet_run` tool and the daemon both launch runs that can
//! sign, so a run that cannot was not launched by either: stop it here,
//! before a job is queued or a member is delegated.

use std::path::Path;

use anyhow::{Result, bail};
use mur_common::fleet::Fleet;

use crate::channel_writer::{CONTROL_PLANE_AGENT, unsignable_reason};

/// `Ok` when this process can sign as both identities a run of `fleet`
/// writes under: the control plane and the fleet's router.
pub(crate) fn require_signing(mur_home: &Path, fleet: &Fleet) -> Result<()> {
    check(fleet, |agent| unsignable_reason(mur_home, agent))
}

fn check(fleet: &Fleet, reason: impl Fn(&str) -> Option<String>) -> Result<()> {
    let router = fleet.router_or_concierge();
    let mut signers = vec![CONTROL_PLANE_AGENT];
    if !router.eq_ignore_ascii_case(CONTROL_PLANE_AGENT) {
        signers.push(router);
    }
    let reasons: Vec<String> = signers.into_iter().filter_map(reason).collect();
    if reasons.is_empty() {
        return Ok(());
    }
    bail!(
        "refusing to run fleet '{}': {}. A run started here could not sign its events or \
         ask for an approval. From inside an agent, start it with the `fleet_run` tool; \
         otherwise run `mur fleet run` from your own terminal.",
        fleet.name,
        reasons.join("; ")
    )
}

#[cfg(test)]
#[path = "sign_preflight_tests.rs"]
mod tests;
