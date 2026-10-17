//! Restarting a damaged channel on a fresh hash chain (#1965), as ONE
//! critical section under the channel's append lock:
//!
//! `lock` → move the log aside → `prepare` (drop the old heads, build the
//! repair record) → write the record, SIGNED, as seq 0 → seal the new head
//! → `unlock`.
//!
//! Holding the lock across all of it is the point. If it were released after
//! the move, another writer could append old, still-valid records to the
//! fresh log before the repair record lands, and the restart would then seal
//! a head over them — trusting exactly the history it meant to discard.
//!
//! The record is always signed: there is no unsigned fallback here. A
//! restart that cannot sign would drop the old log and head and leave a
//! chain nothing can verify, so the caller must hold the key up front.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
use mur_common::identity::AgentIdentity;

use super::ChannelService;
use super::anchored::HeadSigner;

/// Who signs the fresh chain's first line.
pub struct RestartSigner<'a> {
    pub identity: &'a AgentIdentity,
    pub kv: u32,
    /// `Some(agent_home)` also seals the router head in `channel-heads/`
    /// (the trusted, outside-the-sandbox writer). `None` leaves that head
    /// absent and relies on the service's own seal hook (Gate B's agent head).
    pub anchor_home: Option<&'a Path>,
}

/// What a restart did.
#[derive(Debug)]
pub struct Restarted {
    pub quarantined: Option<PathBuf>,
    pub event: ChannelEvent,
}

impl ChannelService {
    /// Quarantine `channel_id`'s log and open a fresh chain with one signed
    /// record, all under one append-lock acquisition (see the module docs).
    ///
    /// `prepare` runs under the lock after the move, with the quarantined
    /// path; it drops whatever heads must go and returns the record payload.
    /// An `Err` from it aborts before anything is written to the new log.
    pub fn restart_chain(
        &self,
        channel_id: &str,
        now: DateTime<Utc>,
        actor: ChannelActor,
        signer: RestartSigner<'_>,
        prepare: impl FnOnce(Option<&Path>) -> Result<serde_json::Value>,
    ) -> Result<Restarted> {
        let head_signer = signer.anchor_home.map(|key_home| HeadSigner {
            identity: signer.identity,
            kv: signer.kv,
            key_home,
        });
        let store = &self.store;
        let (quarantined, event) = store.with_append_lock(channel_id, || {
            let quarantined = store.quarantine_locked(channel_id, now)?;
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
            Ok((quarantined, event))
        })?;
        // Seqs restart at 0; the read-model's fold guard (`seq > last_seq`)
        // would ignore the fresh chain, so drop the row and let the refresh
        // re-seed it.
        if let Err(e) = self.index.remove(channel_id) {
            tracing::warn!(channel_id, error = %e, "could not reset the channel index row");
        }
        self.after_write(channel_id, &event, None);
        Ok(Restarted { quarantined, event })
    }
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
        let ch = svc.create_for_workflow("restart").unwrap().id;
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
                },
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
            .unwrap();

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
        let ch = svc.create_for_workflow("restart").unwrap().id;
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
            },
            |_| anyhow::bail!("no"),
        );
        assert!(err.is_err());
        assert!(!svc.store().events_path(&ch).unwrap().exists());
    }
}
