//! Which request a response answers (#1764, fix item 2).
//!
//! A `HitlResponse` decides nothing on its own: it is the answer to one
//! router-signed `HitlRequest`, named by `hitl_id`, and it must echo that
//! request's `action_hash`. Otherwise an answer could name a request nobody
//! wrote, or approve a harmless request while carrying the hash of something
//! else.
//!
//! Only signed fields are used here: `hitl_id`, `action_hash`, the router's
//! signature, and the signed `issued_at` on both payloads (#1764 option C).
//! Line order and the store's `ts` are not signed and are never read. A
//! payload without `issued_at` predates option C; its time is unknown, so it
//! binds nothing (fail closed). The time rules and their clock-skew slack
//! live in `mur_common::hitl::signed_time`.

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use mur_common::channel::{ChannelEvent, EventKind};
use mur_common::hitl::{HitlRequest, HitlResponse, TimeFault, check_answer_time, check_fresh};

/// Every router-signed request in a channel, keyed by `hitl_id`.
pub(super) struct Requests(HashMap<String, Issued>);

struct Issued {
    action_hash: String,
    /// The request's signed issue time; `None` for a pre-option-C request.
    issued_at: Option<DateTime<Utc>>,
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
    /// The request or the response carries no signed `issued_at`.
    Legacy,
    /// The signed times rule the answer out (early, late, or future-dated).
    Time(TimeFault),
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
                    issued_at: q.issued_at,
                    reissued: false,
                });
        }
        Self(map)
    }

    /// `Ok(answered_at)` if `r` answers exactly the request it names, in
    /// time, judged at `now`. `answered_at` is the response's signed
    /// `issued_at` — the only time a caller may order or age it by.
    pub(super) fn bind(
        &self,
        r: &HitlResponse,
        now: DateTime<Utc>,
    ) -> Result<DateTime<Utc>, Unbound> {
        let issued = self.0.get(&r.hitl_id).ok_or(Unbound::NoRequest)?;
        if issued.reissued {
            return Err(Unbound::Reissued);
        }
        if issued.action_hash != r.action_hash {
            return Err(Unbound::HashMismatch);
        }
        let (Some(asked), Some(answered)) = (issued.issued_at, r.issued_at) else {
            return Err(Unbound::Legacy);
        };
        check_answer_time(asked, answered, now).map_err(Unbound::Time)?;
        Ok(answered)
    }

    /// True if `hitl_id` was signed more than once. Such an id is never
    /// offered as the pending request to answer.
    pub(super) fn is_reissued(&self, hitl_id: &str) -> bool {
        self.0.get(hitl_id).is_some_and(|i| i.reissued)
    }

    /// True if an answer written `now` to `hitl_id` could still bind: signed
    /// once, with a signed `issued_at` that has not expired. Only such a
    /// request is offered as pending; anything else the gate asks afresh,
    /// rather than parking on a question nobody can validly settle.
    pub(super) fn is_answerable(&self, hitl_id: &str, now: DateTime<Utc>) -> bool {
        self.0.get(hitl_id).is_some_and(|i| {
            !i.reissued && i.issued_at.is_some_and(|t| check_fresh(t, now).is_ok())
        })
    }
}
