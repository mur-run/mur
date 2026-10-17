//! Gate A's read of the channel log, through the integrity check (#1935).
//!
//! A per-line signature proves who wrote a line, not that the log still holds
//! every line it had. Deleting a deny, deleting the gate's own request, or
//! cutting the log back below the signed head all leave lines that verify one
//! by one. What they break is the hash chain (`chain_breaks`) and the signed
//! head in `channel-heads/` (`anchor_mismatch`, `anchor_dirty`), so those —
//! plus lines that do not parse — are what the gate treats as damage.
//!
//! `DamageReport::is_clean` is deliberately NOT used: its `unverified` list is
//! a one-key check, and an agent's events are signed by that agent, so every
//! agent line would count as damage. Per-event authority is already checked
//! where it matters (`super::authority`).
//!
//! Gate B (the runtime's in-sandbox gate) checks `hitl-heads/` only, not
//! `channel-heads/`: it runs inside the agent's own sandbox, and a compromised
//! agent is out of scope there (`mur-agent-runtime/src/hitl/integrity.rs`).

use std::path::Path;

use anyhow::Result;
use mur_channel::ChannelService;
use mur_channel::store::{AnchorMismatchKind, DamageReport};
use mur_common::channel::{ChannelActor, ChannelEvent};

/// Stands in for the router key on a keyless home. Not a key: with no head on
/// disk the anchor check never reads it.
const NO_ROUTER_KEY: [u8; 32] = [0u8; 32];

/// Prefix of every reason this module produces. Tests and operators match on
/// it, so it is one constant.
pub(super) const INTEGRITY_FAILED: &str = "log integrity check failed";

/// The log's events, and — when the log cannot be trusted — why.
pub(super) struct Checked {
    pub events: Vec<ChannelEvent>,
    pub damage: Option<String>,
}

/// Read `channel_id`'s events with the integrity check, as one locked
/// snapshot (so a concurrent anchored append is not mistaken for damage).
pub(super) fn load_checked(mur_home: &Path, channel_id: &str) -> Result<Checked> {
    let svc = ChannelService::open(mur_home)?;
    let router_key = crate::channel_verify::actor_pubkey(mur_home, &ChannelActor::System, None);
    // Signatures are not judged here (see module doc), so `require_sig` is off.
    let (events, report) = svc.store().load_events_with_damage_locked(
        channel_id,
        &router_key.unwrap_or(NO_ROUTER_KEY),
        false,
    )?;
    drop(svc);
    let damage = describe(&report, router_key.is_some());
    if let Some(why) = &damage {
        tracing::warn!(channel_id, damage = %why, "HITL gate: {INTEGRITY_FAILED}");
    }
    Ok(Checked { events, damage })
}

/// The damage that decides, as one human-readable line; `None` when clean.
///
/// Without the router key there is nothing to check a head against, so only
/// the chain is checked — unless a head is on disk anyway: an anchored channel
/// whose head cannot be verified is not trusted.
fn describe(r: &DamageReport, have_router_key: bool) -> Option<String> {
    let mut parts = Vec::new();
    if !r.chain_breaks.is_empty() {
        parts.push(format!("hash chain broken at seq {:?}", r.chain_breaks));
    }
    if !r.unparseable_lines.is_empty() {
        parts.push(format!("unparseable lines {:?}", r.unparseable_lines));
    }
    if let Some(m) = &r.anchor_mismatch {
        parts.push(if have_router_key {
            format!("signed head does not hold ({})", kind_str(m.kind))
        } else {
            "a signed head exists but the router key to check it cannot be read".to_string()
        });
    }
    if r.anchor_dirty {
        parts.push("signed head is stale (dirty marker set)".to_string());
    }
    (!parts.is_empty()).then(|| format!("{INTEGRITY_FAILED}: {}", parts.join("; ")))
}

fn kind_str(k: AnchorMismatchKind) -> &'static str {
    match k {
        AnchorMismatchKind::BadSignature => "bad signature",
        AnchorMismatchKind::WrongChannel => "names another channel",
        AnchorMismatchKind::Truncated => "log truncated below the head",
        AnchorMismatchKind::Rewritten => "log rewritten inside the head",
    }
}
