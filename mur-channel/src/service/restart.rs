//! Restarting a damaged channel on a fresh hash chain (#1965), as ONE
//! critical section under the channel's append lock:
//!
//! `lock` → recheck (still damaged?) → put down the repair marker → move the
//! log aside → `prepare` (drop the old heads, build the repair record) →
//! write the record, SIGNED, as seq 0 → seal the new head → clear the marker
//! → `unlock`.
//!
//! The recheck runs under the lock because the caller's first check did not:
//! a second repair racing the first would otherwise quarantine the fresh
//! chain the first had just written. The marker (`repair_marker`) covers the
//! one thing the lock cannot: this process dying part-way, which would leave
//! an empty, headless log that reads as a brand-new intact channel.
//!
//! Holding the lock across all of it is the point. If it were released after
//! the move, another writer could append old, still-valid records to the
//! fresh log before the repair record lands, and the restart would then seal
//! a head over them — trusting exactly the history it meant to discard.
//!
//! A channel that had a live owner record keeps its owner (C5): with a
//! [`RestartOwner`] the human-signed `owner-key-enrolled` (`human_cut: None`)
//! is the chain's second line, written inside the SAME acquisition and before
//! the marker clears, so a crash between the two lines leaves the marker down
//! and a rerun redoes both. `human_cut: None` loses no trust: everything
//! before the restart lives only in the quarantined log, which no reader
//! loads, and the new chain starts at seq 0 with the router-signed repair
//! record, so no keyless `Human` line precedes the enrollment.
//!
//! The record is always signed: there is no unsigned fallback here. A
//! restart that cannot sign would drop the old log and head and leave a
//! chain nothing can verify, so the caller must hold the key up front.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind, OwnerKeyEnrolledPayload};
use mur_common::identity::human::rotation_chain;
use mur_common::identity::{AgentIdentity, encode_pubkey_bytes};

use super::ChannelService;
use super::anchored::HeadSigner;
use super::create::{HumanSign, RouterSeal, router_head};
use crate::store::ManifestPolicy;
use crate::verify::human::HumanTrust;

/// Who signs the fresh chain's first line.
pub struct RestartSigner<'a> {
    pub identity: &'a AgentIdentity,
    pub kv: u32,
    /// `Some(agent_home)` also seals the router head in `channel-heads/`
    /// (the trusted, outside-the-sandbox writer). `None` leaves that head
    /// absent and relies on the service's own seal hook (Gate B's agent head).
    pub anchor_home: Option<&'a Path>,
    /// Carry the human owner onto the fresh chain (C5). `Some` only for a
    /// channel that had a live owner record: a legacy channel restarts
    /// exactly as before and is enrolled, if ever, by C1's first human write.
    pub owner: Option<RestartOwner<'a>>,
}

/// The owner a restart re-enrolls. The repair record must be signed by the
/// router (`RestartSigner::identity` with `anchor_home`), so that one key
/// seals every head of the fresh chain, as on any owned channel (H1).
#[derive(Clone, Copy)]
pub struct RestartOwner<'a> {
    pub signer: &'a dyn HumanSign,
    pub router: RouterSeal<'a>,
    /// A0 is in force (see `EnrollGate::identity_protected`, D-7).
    pub identity_protected: bool,
}

/// What a restart did.
#[derive(Debug)]
pub struct Restarted {
    pub quarantined: Option<PathBuf>,
    /// The repair record, at seq 0.
    pub event: ChannelEvent,
    /// The re-enrolled owner record, at seq 1, when an owner was carried over.
    pub owner_record: Option<ChannelEvent>,
}

impl ChannelService {
    /// Quarantine `channel_id`'s log and open a fresh chain with one signed
    /// record, all under one append-lock acquisition (see the module docs).
    ///
    /// `still_damaged` runs first, under the lock: `Ok(false)` means another
    /// repair already finished, and nothing is touched (`Ok(None)`). It must
    /// not take the append lock itself, and must count a pending repair
    /// marker as damage so an interrupted repair is resumed, not skipped.
    ///
    /// `prepare` runs under the lock after the move, with the quarantined
    /// path; it drops whatever heads must go and returns the record payload.
    /// An `Err` from it aborts before anything is written to the new log,
    /// leaving the marker down so the channel stays refused until a rerun.
    pub fn restart_chain(
        &self,
        channel_id: &str,
        now: DateTime<Utc>,
        actor: ChannelActor,
        signer: RestartSigner<'_>,
        still_damaged: impl FnOnce() -> Result<bool>,
        prepare: impl FnOnce(Option<&Path>) -> Result<serde_json::Value>,
    ) -> Result<Option<Restarted>> {
        let owner = signer.owner;
        let head_signer = signer
            .anchor_home
            .map(|key_home| HeadSigner::event(signer.identity, signer.kv, key_home));
        let store = &self.store;
        let mur_home = store.mur_home();
        let done = store.with_append_lock(channel_id, ManifestPolicy::Restore, || {
            if !still_damaged()? {
                return Ok(None);
            }
            // Decided before anything moves, and refused before anything
            // moves: a refusal after the move would leave the channel
            // refused until a rerun.
            let carry = had_live_owner(store, mur_home, channel_id)?;
            let carried = match (carry, owner) {
                (false, _) => None,
                (true, Some(o)) => {
                    check_owner(mur_home, &signer, &o)?;
                    Some(o)
                }
                (true, None) => bail!(
                    "this channel has a human owner, and its fresh chain must carry it over. \
                     Run `mur channel repair` from your own terminal, where the human key and \
                     the router key are both readable; nothing was touched"
                ),
            };
            crate::repair_marker::mark_owned(mur_home, channel_id, &now.to_rfc3339(), carry)?;
            store.quarantine_locked(channel_id, now)?;
            // The ORIGINAL damaged log: on a resumed repair this run found
            // nothing to move, and the marker is what still names it.
            let quarantined = original_quarantine(store, mur_home, channel_id)?;
            let payload = prepare(quarantined.as_deref())?;
            let event = self.with_hooks(head_signer, |hooks| {
                store.append_signed_event_locked(
                    channel_id,
                    signer.identity,
                    signer.kv,
                    actor,
                    EventKind::Note,
                    payload,
                    None,
                    hooks,
                )
            })?;
            if event.seq != 0 {
                // Unreachable while the lock is held across the move; checked
                // because a fresh chain that is not fresh must never pass.
                bail!(
                    "fresh chain's first record landed at seq {}, not 0",
                    event.seq
                );
            }
            // The owner record is the chain's second line, in this same lock
            // and before the marker clears: a crash between the two leaves
            // the marker, and the rerun restarts from the top.
            let owner_record = carried
                .map(|o| self.enroll_restarted(channel_id, &o))
                .transpose()?;
            // The fresh chain and its head are down; only now is the repair
            // done. A crash before this line leaves the marker for a rerun.
            crate::repair_marker::clear(mur_home, channel_id)?;
            Ok(Some((quarantined, event, owner_record)))
        })?;
        let Some((quarantined, event, owner_record)) = done else {
            return Ok(None);
        };
        // Seqs restart at 0; the read-model's fold ledger (`channel_folds`)
        // would treat the fresh chain's seqs as already folded, so drop the
        // row (which clears its ledger) and let the refresh re-seed it.
        if let Err(e) = self.index.remove(channel_id) {
            tracing::warn!(channel_id, error = %e, "could not reset the channel index row");
        }
        self.after_write(channel_id, owner_record.as_ref().unwrap_or(&event), None);
        Ok(Some(Restarted {
            quarantined,
            event,
            owner_record,
        }))
    }

    /// The fresh chain's `owner-key-enrolled` (`human_cut: None`), under the
    /// append lock the caller holds, sealed by the router like every head of
    /// an owned channel. The manifest is already `v: 3`: the channel had a
    /// live record, so it was bumped when that record was written.
    fn enroll_restarted(&self, id: &str, owner: &RestartOwner<'_>) -> Result<ChannelEvent> {
        self.store.update_manifest_locked(id, |_| {})?;
        let key = encode_pubkey_bytes(&owner.signer.pubkey());
        let payload = serde_json::to_value(OwnerKeyEnrolledPayload {
            owner_key: key.clone(),
            key_version: owner.signer.key_version(),
            human_cut: None,
        })?;
        self.with_hooks(Some(router_head(owner.router, false, None)), |hooks| {
            self.store.append_signed_by_locked(
                id,
                owner.signer.key_version(),
                &|m| owner.signer.sign(m),
                ChannelActor::local_human_with_key(key),
                EventKind::OwnerKeyEnrolled,
                payload,
                None,
                hooks,
            )
        })
    }
}

/// Did this channel have a live owner record when the repair began? A pending
/// marker answers for a resumed run (its log is already aside); otherwise the
/// log and head as they stand, under the lock the caller holds.
fn had_live_owner(store: &crate::ChannelStore, mur_home: &Path, id: &str) -> Result<bool> {
    if crate::repair_marker::is_pending(mur_home, id) {
        return Ok(crate::repair_marker::is_owned(mur_home, id));
    }
    let path = store.events_path(id)?;
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    // A head that cannot be read is damage to repair, not a reason to lose
    // the owner: the record's own signature still decides (an unanchored
    // record is live, flagged as damage).
    let head = crate::head_io::read_head(&crate::anchor::heads_dir(mur_home), id)
        .ok()
        .flatten();
    let log = String::from_utf8_lossy(&bytes);
    Ok(HumanTrust::from_log(mur_home, id, &log, head.as_ref())
        .live_record()
        .is_some())
}

/// Everything about carrying an owner over that can be refused BEFORE the
/// log is touched: a refusal after the move would leave the channel refused
/// until a rerun.
fn check_owner(
    mur_home: &Path,
    signer: &RestartSigner<'_>,
    owner: &RestartOwner<'_>,
) -> Result<()> {
    if !owner.identity_protected {
        bail!(
            "refusing to carry the owner over: the sandbox does not protect `identity/` from \
             writes, so the human key chain could be forged (D-7)"
        );
    }
    if signer.anchor_home.is_none()
        || signer.identity.verifying_key_bytes() != owner.router.identity.verifying_key_bytes()
    {
        bail!("an owned channel's fresh chain is signed and sealed by the router key alone");
    }
    let chain = rotation_chain(mur_home).context("read the human rotation chain")?;
    if owner.signer.pubkey() != chain.head_pubkey()
        || owner.signer.key_version() != chain.head_key_version()
    {
        bail!("the signer is not the head of the local human chain; refusing to carry it over");
    }
    Ok(())
}

/// First log the marker names that is still on disk beside the channel.
fn original_quarantine(
    store: &crate::ChannelStore,
    mur_home: &Path,
    channel_id: &str,
) -> Result<Option<PathBuf>> {
    let Some(dir) = store
        .events_path(channel_id)?
        .parent()
        .map(Path::to_path_buf)
    else {
        return Ok(None);
    };
    Ok(crate::repair_marker::quarantined(mur_home, channel_id)
        .into_iter()
        .map(|n| dir.join(n))
        .find(|p| p.is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tempfile::TempDir;

    /// How long `prepare` holds the lock while a racer tries to append. A
    /// racer that is NOT excluded finishes well inside this window.
    const RACE_WINDOW: Duration = Duration::from_millis(300);

    fn note(svc: &ChannelService, ch: &str, n: u64) -> ChannelEvent {
        svc.append(
            ch,
            ChannelActor::System,
            EventKind::Note,
            serde_json::json!({ "n": n }),
            None,
        )
        .unwrap()
    }

    /// Review P1 on #1973: the old repair released the append lock between
    /// the move and the repair record, so a writer could slip an old record
    /// into the fresh log and have it sealed as trusted. Now a racing append
    /// is held off until the signed seq-0 record is down.
    #[test]
    fn a_racing_append_waits_until_the_fresh_chain_is_written() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc
            .create_for_workflow("restart", crate::service::ChannelOwner::Legacy)
            .unwrap()
            .id;
        note(&svc, &ch, 0);
        note(&svc, &ch, 1);
        let identity = AgentIdentity::generate();
        let done = Arc::new(AtomicBool::new(false));
        let home = tmp.path().to_path_buf();
        let mut racer = None;

        let out = svc
            .restart_chain(
                &ch,
                Utc::now(),
                ChannelActor::System,
                RestartSigner {
                    identity: &identity,
                    kv: 1,
                    anchor_home: None,
                    owner: None,
                },
                || Ok(true),
                |_| {
                    let (home, id, flag) = (home.clone(), ch.clone(), done.clone());
                    racer = Some(std::thread::spawn(move || {
                        let svc = ChannelService::open(&home).unwrap();
                        let ev = note(&svc, &id, 99);
                        flag.store(true, Ordering::SeqCst);
                        ev
                    }));
                    std::thread::sleep(RACE_WINDOW);
                    assert!(
                        !done.load(Ordering::SeqCst),
                        "a writer appended while the restart held the lock"
                    );
                    Ok(serde_json::json!({ "type": "channel-repair" }))
                },
            )
            .unwrap()
            .expect("a damaged channel is restarted");

        assert_eq!(out.event.seq, 0);
        assert!(out.event.sig.is_some());
        let racer_ev = racer.unwrap().join().unwrap();
        assert_eq!(racer_ev.seq, 1, "the racer lands after the repair record");
        let events = svc.store().load_events(&ch).unwrap();
        assert_eq!(events[0].payload["type"], "channel-repair");
        assert!(out.quarantined.is_some());
    }

    /// `prepare` failing writes nothing to the fresh log.
    #[test]
    fn a_failed_prepare_writes_no_record() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc
            .create_for_workflow("restart", crate::service::ChannelOwner::Legacy)
            .unwrap()
            .id;
        note(&svc, &ch, 0);
        let identity = AgentIdentity::generate();
        let err = svc.restart_chain(
            &ch,
            Utc::now(),
            ChannelActor::System,
            RestartSigner {
                identity: &identity,
                kv: 1,
                anchor_home: None,
                owner: None,
            },
            || Ok(true),
            |_| anyhow::bail!("no"),
        );
        assert!(err.is_err());
        assert!(!svc.store().events_path(&ch).unwrap().exists());
        // Stopped part-way: the marker stays, so the channel reads as
        // damaged rather than as a fresh, intact one.
        assert!(crate::repair_marker::is_pending(tmp.path(), &ch));
        assert!(
            !svc.store()
                .load_events_with_damage(&ch, &[0; 32], false)
                .unwrap()
                .1
                .is_clean()
        );
    }

    /// Review P1 on #1968: the caller's integrity check runs before the
    /// lock. When the recheck under the lock finds the channel healthy
    /// (another repair finished first), nothing is moved or written.
    #[test]
    fn a_recheck_that_finds_no_damage_touches_nothing() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc
            .create_for_workflow("restart", crate::service::ChannelOwner::Legacy)
            .unwrap()
            .id;
        note(&svc, &ch, 0);
        let before = std::fs::read(svc.store().events_path(&ch).unwrap()).unwrap();
        let identity = AgentIdentity::generate();
        let out = svc
            .restart_chain(
                &ch,
                Utc::now(),
                ChannelActor::System,
                RestartSigner {
                    identity: &identity,
                    kv: 1,
                    anchor_home: None,
                    owner: None,
                },
                || Ok(false),
                |_| panic!("prepare must not run when the recheck finds no damage"),
            )
            .unwrap();
        assert!(out.is_none());
        assert_eq!(
            std::fs::read(svc.store().events_path(&ch).unwrap()).unwrap(),
            before
        );
        assert!(!crate::repair_marker::is_pending(tmp.path(), &ch));
    }

    /// The recheck really runs under the append lock: a writer racing it is
    /// held off until the restart is done.
    #[test]
    fn the_recheck_runs_under_the_append_lock() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc
            .create_for_workflow("restart", crate::service::ChannelOwner::Legacy)
            .unwrap()
            .id;
        note(&svc, &ch, 0);
        let identity = AgentIdentity::generate();
        let done = Arc::new(AtomicBool::new(false));
        let home = tmp.path().to_path_buf();
        let mut racer = None;
        svc.restart_chain(
            &ch,
            Utc::now(),
            ChannelActor::System,
            RestartSigner {
                identity: &identity,
                kv: 1,
                anchor_home: None,
                owner: None,
            },
            || {
                let (home, id, flag) = (home.clone(), ch.clone(), done.clone());
                racer = Some(std::thread::spawn(move || {
                    let svc = ChannelService::open(&home).unwrap();
                    note(&svc, &id, 99);
                    flag.store(true, Ordering::SeqCst);
                }));
                std::thread::sleep(RACE_WINDOW);
                assert!(
                    !done.load(Ordering::SeqCst),
                    "a writer appended while the recheck held the lock"
                );
                Ok(false)
            },
            |_| unreachable!(),
        )
        .unwrap();
        racer.unwrap().join().unwrap();
        assert!(done.load(Ordering::SeqCst));
    }
}

#[cfg(test)]
#[path = "restart_owner_tests.rs"]
mod owner_tests;
