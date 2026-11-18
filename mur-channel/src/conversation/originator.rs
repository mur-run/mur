//! Who a channel turn's event came from, judged by its signature (E1).
//!
//! The same rule the HITL gate uses for authority
//! (`mur_core::hitl::authority::is_human_authority`), asked of the one event
//! a turn answers: a `Human` actor is the human only when the channel's owner
//! trust and the human chain's **head** key (D-9) say so. Anything else that
//! claims to be human is [`Originator::UntrustedHumanClaim`] and counts as
//! agent-originated.
//!
//! Reads `identity/` (public) and the router's public key only — never
//! `keys/`, which every sandboxed runtime is denied.
use std::path::Path;

use mur_common::channel::{ChannelActor, ChannelEvent};
use mur_common::identity::decode_pubkey;
use mur_common::identity::human::RotationChain;

use crate::verify::human::HumanTrust;

/// Who wrote a turn's event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Originator {
    /// The human. `key: Some` = signed by the head key of the local human
    /// chain; `None` = a keyless router-signed line the channel still
    /// accepts (no owner record, or at/before the record's `human_cut`).
    Human { key: Option<String> },
    /// An agent, by its own actor id.
    Agent { id: String },
    /// A `System` line (murmur's `!cmd` card, a router note). Not a human
    /// claim, so it carries no human standing either.
    System,
    /// A `Human` actor the signature does not back. Agent-originated.
    UntrustedHumanClaim { reason: ClaimReason },
}

impl Originator {
    /// True unless the human wrote it.
    pub fn is_agent_originated(&self) -> bool {
        !matches!(self, Self::Human { .. })
    }
}

/// Why a human claim was not believed. Kept apart so an audit can tell a
/// forgery from policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimReason {
    /// No signature, or one that does not verify under the key it names
    /// (or, keyless, under the router).
    SignatureInvalid,
    /// A valid signature by a key the human chain attests but has since
    /// retired (D-9: only the head key speaks).
    KeyVersionStale,
    /// A valid signature that carries no standing here: a keyless router
    /// line past the owner record's `human_cut`, or a head-key line on a
    /// channel with no live owner record.
    NoStanding,
}

impl ClaimReason {
    /// The audit string.
    pub fn code(self) -> &'static str {
        match self {
            Self::SignatureInvalid => "SIGNATURE_INVALID",
            Self::KeyVersionStale => "KEY_VERSION_STALE",
            Self::NoStanding => "NO_STANDING",
        }
    }
}

/// Classify `ev`. `chain` is the verified human chain, `None` when no human
/// is enrolled or the chain does not verify. The caller has already refused
/// a channel whose owner state is damaged.
pub fn classify(
    mur_home: &Path,
    channel_id: &str,
    trust: &HumanTrust,
    chain: Option<&RotationChain>,
    ev: &ChannelEvent,
) -> Originator {
    let key = match &ev.actor {
        ChannelActor::Agent { id } => return Originator::Agent { id: id.clone() },
        ChannelActor::System => return Originator::System,
        ChannelActor::Human { key, .. } => key,
    };
    let untrusted = |reason| Originator::UntrustedHumanClaim { reason };
    if ev.sig.is_none() {
        return untrusted(ClaimReason::SignatureInvalid);
    }
    let live = trust.live_record();
    match key {
        Some(k) => {
            let (Some(chain), Ok(named)) = (chain, decode_pubkey(k)) else {
                return untrusted(ClaimReason::SignatureInvalid);
            };
            // The named key picks the key tried; `ev.key_version` never does.
            if named == chain.head_pubkey() {
                if !crate::sign::verify_one(channel_id, ev, &named, true) {
                    untrusted(ClaimReason::SignatureInvalid)
                } else if live.is_none() {
                    untrusted(ClaimReason::NoStanding)
                } else {
                    Originator::Human {
                        key: Some(k.clone()),
                    }
                }
            } else if retired_on_chain(chain, &named)
                && crate::sign::verify_one(channel_id, ev, &named, true)
            {
                untrusted(ClaimReason::KeyVersionStale)
            } else {
                untrusted(ClaimReason::SignatureInvalid)
            }
        }
        None => {
            let router_signed = crate::verify::router_pubkey(mur_home, ev.key_version)
                .is_some_and(|pk| crate::sign::verify_one(channel_id, ev, &pk, true));
            if !router_signed {
                untrusted(ClaimReason::SignatureInvalid)
            } else if live.is_none_or(|l| {
                l.payload
                    .human_cut
                    .as_ref()
                    .is_some_and(|cut| ev.seq <= cut.seq)
            }) {
                Originator::Human { key: None }
            } else {
                untrusted(ClaimReason::NoStanding)
            }
        }
    }
}

/// `key` is an earlier, non-head key the verified chain attests.
fn retired_on_chain(chain: &RotationChain, key: &[u8; 32]) -> bool {
    chain
        .entries()
        .iter()
        .filter_map(|a| decode_pubkey(&a.new_pubkey).ok())
        .any(|k| &k == key && k != chain.head_pubkey())
}
