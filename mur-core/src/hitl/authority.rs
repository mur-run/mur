//! Who may answer a HITL gate, and which questions may be answered.
//!
//! A verified signature proves who WROTE an event, not that the writer may
//! decide. Every agent can write `channels/` and each can sign as itself, so
//! `channel_verify::verify_event` — which checks an event against its own
//! actor's key and, with `MUR_CHANNEL_REQUIRE_SIG` unset, accepts an unsigned
//! one — let an agent approve its own gated action. Approvals, and the
//! requests they answer, are held to a stricter rule that does not depend on
//! that variable: only the router speaks for the human.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use mur_channel::verify::human::HumanTrust;
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
use mur_common::hitl::HitlRequest;
use mur_common::identity::{RotationReason, decode_pubkey};

/// Everything the predicates below need about one channel, loaded once per
/// scan (C3): the channel's owner trust, the human chain's head key, and the
/// router keys a durable record may verify under. No predicate reloads
/// anything per event.
pub struct AuthorityCtx {
    mur_home: PathBuf,
    channel_id: String,
    trust: HumanTrust,
    /// The local human chain's current key. The only human key that carries
    /// authority (D-9); `None` when no human is enrolled or the chain does
    /// not verify.
    human_head: Option<[u8; 32]>,
    /// The router's head key plus every earlier key the verified rotation
    /// chain retired routinely (D-10, A1). Head-only when the chain is
    /// missing or does not verify.
    durable_router_keys: Vec<[u8; 32]>,
}

impl AuthorityCtx {
    /// Read `channel_id`'s raw log and signed head under the shared lock and
    /// build the context. For a caller that already holds the raw bytes of
    /// its own read, [`AuthorityCtx::from_log`] reuses them instead.
    pub fn load(mur_home: &Path, channel_id: &str) -> Result<Self> {
        let store = mur_channel::ChannelStore::new(mur_home);
        let (raw, head) = store.with_shared_lock(channel_id, |s| {
            let path = s.events_path(channel_id)?;
            let raw = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(e) => return Err(e.into()),
            };
            let head = mur_channel::head_io::read_head(
                &mur_channel::anchor::heads_dir(mur_home),
                channel_id,
            )?;
            Ok((raw, head))
        })?;
        Ok(Self::from_log(mur_home, channel_id, &raw, head.as_ref()))
    }

    /// Build from bytes the caller already read (one load for the gate).
    pub fn from_log(
        mur_home: &Path,
        channel_id: &str,
        raw: &str,
        head: Option<&mur_channel::head::Head>,
    ) -> Self {
        Self {
            mur_home: mur_home.to_path_buf(),
            channel_id: channel_id.to_string(),
            trust: HumanTrust::from_log(mur_home, channel_id, raw, head),
            human_head: mur_common::identity::human::rotation_chain(mur_home)
                .ok()
                .map(|c| c.head_pubkey()),
            durable_router_keys: durable_router_keys(mur_home),
        }
    }
}

/// D-10 (A1): the router's head key, then every earlier key in the verified
/// rotation chain whose retirement was routine (`scheduled`,
/// `owner-change`). A key retired by `suspect-compromise` or `emergency` is
/// never a candidate. A chain that does not verify, or whose head is not the
/// key the router resolves to, yields the head key only (D-9), never "all
/// keys".
fn durable_router_keys(mur_home: &Path) -> Vec<[u8; 32]> {
    let Some(head) = mur_channel::verify::router_pubkey(mur_home, None) else {
        return Vec::new();
    };
    let mut keys = vec![head];
    let router_home = mur_home
        .join("agents")
        .join(mur_channel::verify::ROUTER_AGENT);
    let Some(chain) = mur_channel::sign::verified_writer_chain(&router_home) else {
        return keys;
    };
    if chain
        .last()
        .and_then(|a| decode_pubkey(&a.new_pubkey).ok())
        .is_none_or(|k| k != head)
    {
        return keys;
    }
    for pair in chain.windows(2) {
        let (key, retired_by) = (&pair[0], &pair[1]);
        if matches!(
            retired_by.reason,
            RotationReason::Scheduled | RotationReason::OwnerChange
        ) && let Ok(k) = decode_pubkey(&key.new_pubkey)
            && !keys.contains(&k)
        {
            keys.push(k);
        }
    }
    keys
}

/// True if the router wrote `ev`: a Human or System actor (an agent never
/// speaks for the human), a signature, and that signature verifying against
/// the router's key.
///
/// This proves MUR asked a question or recorded an answer. It does NOT prove a
/// human decided anything: the gate's own `--yes` / tier-grant audit record is
/// router-signed too, as `System`. Use [`is_human_authority`] for that.
///
/// No `require_sig` fallback, on purpose. An unsigned event is exactly what a
/// sandboxed `mur channel approve` writes — it cannot read the router key — so
/// tolerating unsigned approvals is tolerating agent-written ones. The cost: a
/// home with no router identity cannot approve anything.
pub fn is_router_signed(ctx: &AuthorityCtx, ev: &ChannelEvent) -> bool {
    if matches!(ev.actor, ChannelActor::Agent { .. }) || ev.sig.is_none() {
        return false;
    }
    mur_channel::verify::router_pubkey(&ctx.mur_home, ev.key_version)
        .is_some_and(|pk| mur_channel::sign::verify_one(&ctx.channel_id, ev, &pk, true))
}

/// A router-written record with no TTL (D-10): today only the `run_paused`
/// note resume reads. Verifies under the router's head key or any key the
/// verified chain retired routinely. Every candidate is tried;
/// `ev.key_version` is never consulted, so a label cannot pick the key.
pub fn is_durable_router_record(ctx: &AuthorityCtx, ev: &ChannelEvent) -> bool {
    if matches!(ev.actor, ChannelActor::Agent { .. }) || ev.sig.is_none() {
        return false;
    }
    ctx.durable_router_keys
        .iter()
        .any(|pk| mur_channel::sign::verify_one(&ctx.channel_id, ev, pk, true))
}

/// True if `ev` is an answer someone with standing wrote: the router, or a
/// human the channel's owner trust accepts (signature required). Decides
/// which responses the gate looks at; whether one *decides* anything is
/// [`is_human_authority`]. Before C3 the scan kept only router-signed
/// answers, so a deferred human-key approval never reached that check (N4).
pub fn is_authentic_answer(ctx: &AuthorityCtx, ev: &ChannelEvent) -> bool {
    is_router_signed(ctx, ev)
        || ctx
            .trust
            .is_trusted_human(&ctx.mur_home, &ctx.channel_id, ev, true)
}

/// True if `ev` is the human's decision (D-4, D-9). Never on a channel
/// whose owner state is damaged.
///
/// - Keyed `Human`: the channel has a live owner record, the actor names the
///   human chain's **head** key, and the signature verifies under that key
///   and nothing else — never a key picked by `ev.key_version`. A pre-rekey
///   approval still projects as a trusted human event (provenance) but
///   grants nothing here.
/// - Keyless `Human`: router-signed (`mur channel approve`, the phone) and,
///   on an owned channel, at or before `human_cut` — past the cut a keyless
///   human line is exactly what the router writes for anyone who asks it.
///
/// A `System` answer is the gate recording what it did on its own — `--yes`
/// or a pre-approved tier. That record is audit, not consent: counting it
/// would let one `--yes` run approve every later run of the same action for
/// the whole approval TTL, with or without `--yes` (#1764).
pub fn is_human_authority(ctx: &AuthorityCtx, ev: &ChannelEvent) -> bool {
    let ChannelActor::Human { key, .. } = &ev.actor else {
        return false;
    };
    // Owner damage (an unanchored record, a cut past the head or off its
    // line) means the owner state itself is in question: no human decides.
    if !ctx.trust.damage().is_empty() {
        return false;
    }
    let live = ctx.trust.live_record();
    match key {
        Some(k) => {
            let Some(head) = ctx.human_head else {
                return false;
            };
            live.is_some()
                && ev.sig.is_some()
                && decode_pubkey(k).is_ok_and(|named| named == head)
                && mur_channel::sign::verify_one(&ctx.channel_id, ev, &head, true)
        }
        None => {
            is_router_signed(ctx, ev)
                && live.is_none_or(|l| {
                    l.payload
                        .human_cut
                        .as_ref()
                        .is_some_and(|cut| ev.seq <= cut.seq)
                })
        }
    }
}

/// The request `hitl_id` names, if the router asked it.
///
/// A response echoes its request's `action_hash`, and the gate releases on
/// that hash. Answering a request an agent wrote would let it put a harmless
/// `summary` in front of the hash of something else. The newest router-signed
/// request with this id wins; one the router did not sign is never answered.
pub fn request_to_answer(
    mur_home: &Path,
    channel_id: &str,
    events: &[ChannelEvent],
    hitl_id: &str,
) -> Result<HitlRequest> {
    let ctx = AuthorityCtx::load(mur_home, channel_id)?;
    let mut unsigned_match = false;
    for e in events
        .iter()
        .rev()
        .filter(|e| e.kind == EventKind::HitlRequest)
    {
        let Ok(r) = serde_json::from_value::<HitlRequest>(e.payload.clone()) else {
            continue;
        };
        if r.hitl_id != hitl_id {
            continue;
        }
        if is_router_signed(&ctx, e) {
            // Refuse what the gate would ignore, rather than write an answer
            // that silently settles nothing (#1764 option C): a request with
            // no signed `issued_at`, or one past its TTL, is not open.
            let Some(asked) = r.issued_at else {
                bail!(
                    "HitlRequest {hitl_id} in channel {channel_id} has no signed issue time \
                     (written before MUR signed HITL times) — it can no longer be answered; \
                     re-run the action to be asked again"
                );
            };
            if let Err(why) = mur_common::hitl::check_fresh(asked, chrono::Utc::now()) {
                bail!(
                    "HitlRequest {hitl_id} in channel {channel_id} can no longer be answered \
                     ({why:?}); re-run the action to be asked again"
                );
            }
            return Ok(r);
        }
        unsigned_match = true;
    }
    if unsigned_match {
        bail!(
            "HitlRequest {hitl_id} in channel {channel_id} is not signed by MUR's router — \
             refusing to answer a question MUR did not ask"
        );
    }
    bail!("no pending HitlRequest {hitl_id} in channel {channel_id}")
}

#[cfg(test)]
#[path = "authority_tests.rs"]
mod tests;
