//! Which request a response answers (#1764, fix item 2).
//!
//! A `HitlResponse` decides nothing on its own: it is the answer to one
//! router-signed `HitlRequest`, named by `hitl_id`, and it must echo that
//! request's `action_hash`. Otherwise an answer could name a request nobody
//! wrote, or approve a harmless request while carrying the hash of something
//! else.
//!
//! Only signed fields are used here (`hitl_id`, `action_hash`, and the
//! router's signature). Line order and `ts` are not signed, so the
//! "answered before it was asked" and expiry checks wait on the #1764
//! decision about signed time.

use std::collections::HashMap;
use std::path::Path;

use mur_common::channel::{ChannelEvent, EventKind};
use mur_common::hitl::{HitlRequest, HitlResponse};

/// Every router-signed request in a channel, keyed by `hitl_id`.
pub(super) struct Requests(HashMap<String, Issued>);

struct Issued {
    action_hash: String,
    /// Signed more than once under one id. The gate mints a fresh UUIDv7 per
    /// request, so a second one is a bug or a replayed line; either way no
    /// answer to that id can be trusted to mean the action it names.
    reissued: bool,
}

/// Why a response does not count as the answer to its request.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Unbound {
    /// No router-signed request carries this `hitl_id`.
    NoRequest,
    /// The request was signed more than once under this id.
    Reissued,
    /// The response's `action_hash` is not the request's.
    HashMismatch,
}

impl Requests {
    pub(super) fn collect(mur_home: &Path, channel_id: &str, events: &[ChannelEvent]) -> Self {
        let mut map: HashMap<String, Issued> = HashMap::new();
        for e in events {
            if e.kind != EventKind::HitlRequest
                || !super::super::authority::is_router_signed(mur_home, channel_id, e)
            {
                continue;
            }
            let Ok(q) = serde_json::from_value::<HitlRequest>(e.payload.clone()) else {
                continue;
            };
            map.entry(q.hitl_id)
                .and_modify(|i| i.reissued = true)
                .or_insert(Issued {
                    action_hash: q.action_hash,
                    reissued: false,
                });
        }
        Self(map)
    }

    /// `Ok` if `r` answers exactly the request it names.
    pub(super) fn bind(&self, r: &HitlResponse) -> Result<(), Unbound> {
        let issued = self.0.get(&r.hitl_id).ok_or(Unbound::NoRequest)?;
        if issued.reissued {
            return Err(Unbound::Reissued);
        }
        if issued.action_hash != r.action_hash {
            return Err(Unbound::HashMismatch);
        }
        Ok(())
    }

    /// True if `hitl_id` was signed more than once. Such an id is never
    /// offered as the pending request to answer.
    pub(super) fn is_reissued(&self, hitl_id: &str) -> bool {
        self.0.get(hitl_id).is_some_and(|i| i.reissued)
    }
}
