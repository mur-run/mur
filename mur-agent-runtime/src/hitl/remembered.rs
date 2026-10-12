//! Fold gate B's channel into the decisions it is currently replaying (#1806).
//!
//! One pass shared by the gate's lookup and by `hitl/list`, so what the user
//! is shown and what the gate does cannot drift apart. Rules, all on signed
//! data only:
//!
//! - a `HitlResponse` counts if it verifies against the agent key, carries a
//!   signed `issued_at`, and is inside `APPROVAL_TTL_SECS`;
//! - a revocation (`EventKind::Note` carrying `REVOCATION_PAYLOAD_KEY`) counts
//!   if it verifies; it cancels every covered decision whose `issued_at` is
//!   not after its own. A decision recorded later stands again;
//! - per hash, the newest surviving decision wins; a deny wins a tie.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use mur_common::channel::{ChannelEvent, EventKind};
use mur_common::hitl::{HitlResponse, HitlRevocation, check_fresh};
use serde::Serialize;

use super::store::Settled;

/// One decision gate B would replay right now.
#[derive(Debug, Clone, Serialize)]
pub struct LiveDecision {
    pub action_hash: String,
    pub allow: bool,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// `None` = recorded before #1806; shown as hash only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub surface: String,
}

impl LiveDecision {
    pub fn settled(&self) -> Settled {
        if self.allow {
            Settled::Allow
        } else {
            Settled::Deny
        }
    }
}

/// Every live decision, keyed by `action_hash`.
pub fn live_decisions(
    channel_id: &str,
    events: &[ChannelEvent],
    pubkey: &[u8; 32],
    now: DateTime<Utc>,
) -> HashMap<String, LiveDecision> {
    // require_sig = true: this channel is only ever written signed, so an
    // unsigned line is not ours — it neither decides nor revokes.
    let verified = |e: &ChannelEvent| mur_channel::sign::verify_one(channel_id, e, pubkey, true);

    let revocations: Vec<HitlRevocation> = events
        .iter()
        .filter(|e| e.kind == EventKind::Note)
        .filter_map(|e| HitlRevocation::from_payload(&e.payload).filter(|_| verified(e)))
        .collect();
    let revoked = |hash: &str, at: DateTime<Utc>| {
        revocations
            .iter()
            .any(|r| r.covers(hash) && at <= r.issued_at)
    };

    let mut live: HashMap<String, LiveDecision> = HashMap::new();
    for e in events.iter().filter(|e| e.kind == EventKind::HitlResponse) {
        let Ok(r) = serde_json::from_value::<HitlResponse>(e.payload.clone()) else {
            continue;
        };
        let Some(at) = r.issued_at else { continue };
        if check_fresh(at, now).is_err() || revoked(&r.action_hash, at) || !verified(e) {
            continue;
        }
        let newer = live
            .get(&r.action_hash)
            .is_none_or(|cur| at > cur.issued_at || (at == cur.issued_at && cur.allow && !r.allow));
        if newer {
            live.insert(
                r.action_hash.clone(),
                LiveDecision {
                    expires_at: at + chrono::Duration::seconds(mur_common::hitl::APPROVAL_TTL_SECS),
                    action_hash: r.action_hash,
                    allow: r.allow,
                    issued_at: at,
                    label: r.label,
                    surface: r.surface,
                },
            );
        }
    }
    live
}
