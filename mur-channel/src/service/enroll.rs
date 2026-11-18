//! C1: an owner record on a legacy channel's first human write (D-3, D-7).
//!
//! Inside one append lock, and only when the channel has no live record:
//! `v: 3` manifest, then the human-signed `owner-key-enrolled` record
//! (`human_cut` = the log's last line, `None` on an empty log), then its
//! router-sealed anchor, then the caller's event. Never from the runtime, a
//! read, or `restart_chain`: those never reach [`ChannelService::append_human`].

use anyhow::{Context, Result, bail};
use mur_common::channel::{
    ChannelActor, ChannelEvent, EventKind, HumanCut, OwnerKeyEnrolledPayload,
};
use mur_common::identity::encode_pubkey_bytes;
use mur_common::identity::human::rotation_chain;

use super::ChannelService;
use super::create::{HumanSign, LegacySealer, RouterSeal, router_head};
use crate::store::{ManifestPolicy, line_hash, line_seq};
use crate::verify::human::HumanTrust;
use crate::{anchor, head_io};

/// What C1 needs the caller to vouch for. Both are decided outside this
/// crate (it must not depend on the runtime or on `mur-core`).
pub struct EnrollGate<'a> {
    /// A0 is in force: the sandbox refuses every write grant over
    /// `<mur_home>/identity` (D-7). Without it an agent could write the human
    /// chain, and ignoring foreign records would be takeover, not lockout.
    pub identity_protected: bool,
    /// The log is intact, run inside the append lock. `SIGNATURE_INVALID`
    /// lines alone are not damage (D-7). Enrolling a damaged log would
    /// launder it under a fresh signed head (N5).
    pub log_intact: &'a dyn Fn() -> Result<bool>,
    /// The agent that sealed the channel's current head, when it is not the
    /// router (a channel anchored before phase 2). Its head is accepted once
    /// and resealed under the router.
    pub legacy_sealer: Option<LegacySealer<'a>>,
}

impl ChannelService {
    /// Enroll `signer` as the channel's owner when it has no live record.
    /// `Ok(None)` when one already exists (nothing is written).
    pub fn enroll_owner_key_if_absent(
        &self,
        channel_id: &str,
        signer: &dyn HumanSign,
        router: RouterSeal<'_>,
        gate: &EnrollGate<'_>,
    ) -> Result<Option<ChannelEvent>> {
        let ev = self
            .store
            .with_append_lock(channel_id, ManifestPolicy::Require, || {
                self.enroll_locked(channel_id, signer, router, gate)
            })?;
        if let Some(ev) = &ev {
            self.after_write(channel_id, ev, None);
        }
        Ok(ev)
    }

    fn enroll_locked(
        &self,
        id: &str,
        signer: &dyn HumanSign,
        router: RouterSeal<'_>,
        gate: &EnrollGate<'_>,
    ) -> Result<Option<ChannelEvent>> {
        let home = self.store.mur_home();
        let path = self.store.events_path(id)?;
        let log = match std::fs::read_to_string(&path) {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        let head = head_io::read_head(&anchor::heads_dir(home), id)?;
        let trust = HumanTrust::from_log(home, id, &log, head.as_ref());
        if trust.live_record().is_some() {
            return Ok(None);
        }
        if let Some(seq) = trust.dormant_records().first() {
            bail!(
                "refusing to enroll an owner: the log holds an owner record at seq {seq} signed \
                 by a retired human key. Sealing a head over it would make it the owner, so \
                 `mur channel repair` this channel first"
            );
        }
        if !gate.identity_protected {
            bail!(
                "refusing to enroll an owner: the sandbox does not protect `identity/` from \
                 writes, so the human key chain could be forged (D-7)"
            );
        }
        let chain = rotation_chain(home).context("read the human rotation chain")?;
        if signer.pubkey() != chain.head_pubkey()
            || signer.key_version() != chain.head_key_version()
        {
            bail!("the signer is not the head of the local human chain; refusing to enroll it");
        }
        if !(gate.log_intact)()? {
            bail!("refusing to enroll an owner on a damaged log; repair it first (N5)");
        }
        // v3 first: a reader that cannot read the record must be locked out
        // before the record exists. Idempotent for a manifest already at v3.
        self.store.update_manifest_locked(id, |_| {})?;
        let human_cut = log
            .lines()
            .rfind(|l| !l.trim().is_empty())
            .map(|l| -> Result<HumanCut> {
                Ok(HumanCut {
                    seq: line_seq(l).context("the log's last line has no seq")?,
                    line_hash: line_hash(l),
                })
            })
            .transpose()?;
        let owner_key = encode_pubkey_bytes(&signer.pubkey());
        let payload = serde_json::to_value(OwnerKeyEnrolledPayload {
            owner_key: owner_key.clone(),
            key_version: signer.key_version(),
            human_cut,
        })?;
        // `live_record: false`: a legacy agent-sealed head is accepted once
        // and resealed under the router.
        let ev = self.with_hooks(
            Some(router_head(router, false, gate.legacy_sealer)),
            |hooks| {
                self.store.append_signed_by_locked(
                    id,
                    signer.key_version(),
                    &|m| signer.sign(m),
                    ChannelActor::local_human_with_key(owner_key),
                    EventKind::OwnerKeyEnrolled,
                    payload,
                    None,
                    hooks,
                )
            },
        )?;
        Ok(Some(ev))
    }

    /// A human-signed, router-anchored event, enrolling the owner first when
    /// the channel has none (C1). The one writer a human event goes through.
    #[allow(clippy::too_many_arguments)]
    pub fn append_human(
        &self,
        channel_id: &str,
        signer: &dyn HumanSign,
        router: RouterSeal<'_>,
        gate: &EnrollGate<'_>,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
    ) -> Result<ChannelEvent> {
        self.enroll_owner_key_if_absent(channel_id, signer, router, gate)?;
        let key = encode_pubkey_bytes(&signer.pubkey());
        let ev = self.with_hooks(Some(router_head(router, true, None)), |hooks| {
            self.store.append_signed_by_with(
                channel_id,
                signer.key_version(),
                &|m| signer.sign(m),
                ChannelActor::local_human_with_key(key),
                kind,
                payload,
                idempotency_key,
                hooks,
            )
        })?;
        self.after_write(channel_id, &ev, None);
        Ok(ev)
    }
}

#[cfg(test)]
#[path = "enroll_tests.rs"]
mod tests;
