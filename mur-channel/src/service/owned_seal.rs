//! H1 for non-human writers: who seals the head of an anchored agent or
//! system write, decided under the append lock from the channel's own state.
//!
//! - No live owner record: the writer seals its own head (today).
//! - Live record, router key readable: the router seals.
//! - Live record, no router key: the line is appended **unanchored** past the
//!   head, as a sandboxed agent's would be. Never under the writer's own key
//!   once a record is live — that head would fail every owned-channel check.
//!
//! A live record the head does not yet cover (a C1 crash between the line and
//! its head) is sealed by the router like any other, as `append_human` does.

use anyhow::{Context, Result};

use super::anchored::{HeadSealer, HeadSigner};
use crate::store::ChannelStore;
use crate::verify::human::HumanTrust;
use crate::{anchor, head_io};

/// The kind tag of an owner record as it appears on a raw line. A log
/// without it has no record, so the full trust pass is skipped.
const OWNER_RECORD_TAG: &str = "\"owner-key-enrolled\"";

/// The signer that actually seals for `signer`, or `None` for an unanchored
/// write. Anything but [`HeadSealer::Writer`] passes through unchanged.
/// Caller must hold the append lock.
pub(super) fn resolve<'a>(
    store: &ChannelStore,
    channel_id: &str,
    signer: HeadSigner<'a>,
) -> Result<Option<HeadSigner<'a>>> {
    let HeadSealer::Writer { router } = signer.sealer else {
        return Ok(Some(signer));
    };
    let event_sealed = HeadSigner {
        sealer: HeadSealer::EventSigner,
        ..signer
    };
    let path = store.events_path(channel_id)?;
    let log = match std::fs::read_to_string(&path) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Some(event_sealed)),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    if !log.contains(OWNER_RECORD_TAG) {
        return Ok(Some(event_sealed));
    }
    let home = store.mur_home();
    let head = head_io::read_head(&anchor::heads_dir(home), channel_id)?;
    let trust = HumanTrust::from_log(home, channel_id, &log, head.as_ref());
    if trust.live_record().is_none() {
        return Ok(Some(event_sealed));
    }
    match router {
        Some(r) => Ok(Some(HeadSigner {
            sealer: HeadSealer::Router {
                identity: r.identity,
                kv: r.kv,
                key_home: r.key_home,
                live_record: true,
            },
            ..signer
        })),
        None => {
            tracing::debug!(
                channel_id,
                "owned channel, no router key: non-human write appended unanchored"
            );
            Ok(None)
        }
    }
}

#[cfg(test)]
#[path = "owned_seal_tests.rs"]
mod tests;
