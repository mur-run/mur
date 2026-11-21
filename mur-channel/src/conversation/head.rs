//! Which key the conversation fold checks the signed head under (H1, D-3
//! rev 4): the router first, always. The writer key — this agent's, at the
//! version its verified rotation history gives — is a fallback for a legacy
//! agent-sealed head, and only while the same load finds no live owner
//! record: once a channel is owned every head is router-sealed, so a head
//! that holds only under the agent's key is one the agent forged.
use anyhow::Result;

use super::Keys;
use crate::head::Head;
use crate::store::{AnchorMismatch, AnchorMismatchKind, ChannelStore};

/// The anchor verdict for `content`. `under_writer` is the verdict the load
/// already computed under the current writer key, reused rather than
/// recomputed. Every check reads `content`; nothing re-reads the log.
pub(super) fn verdict(
    store: &ChannelStore,
    channel_id: &str,
    keys: &Keys<'_>,
    content: &str,
    head: Option<&Head>,
    under_writer: Option<AnchorMismatch>,
    owned: bool,
) -> Result<Option<AnchorMismatch>> {
    let Some(head) = head else {
        return Ok(under_writer);
    };
    let check = |pk: &[u8; 32]| store.anchor_mismatch_for(channel_id, content, pk);
    let bad_sig = |m: &Option<AnchorMismatch>| {
        m.as_ref()
            .is_some_and(|m| m.kind == AnchorMismatchKind::BadSignature)
    };

    // Router: current key, then the key its verified history gives the
    // head's `key_version` (a head sealed before a router rekey).
    let mut router = None;
    for pk in router_keys(keys, head) {
        let m = check(&pk)?;
        if !bad_sig(&m) {
            return Ok(m);
        }
        router = Some(m);
    }
    if owned {
        // No writer fallback. An unreadable router key cannot vouch for the
        // head either: fail closed with the writer verdict's shape.
        return Ok(router.flatten().or(under_writer).or_else(|| {
            Some(AnchorMismatch {
                head: head.clone(),
                kind: AnchorMismatchKind::BadSignature,
                log_lines: crate::head::chain_log(content.as_bytes()).lines,
            })
        }));
    }

    // Legacy: the writer key, then this agent's rotated key for the head.
    if !bad_sig(&under_writer) {
        return Ok(under_writer);
    }
    match super::assess::rotated_key(keys, head.key_version) {
        Some(older) => check(&older),
        None => Ok(under_writer),
    }
}

/// The router's current key and, when different, its strictly resolved key
/// at the head's `key_version`. Empty when the router key cannot be read.
fn router_keys(keys: &Keys<'_>, head: &Head) -> Vec<[u8; 32]> {
    let Some(current) = crate::verify::control_plane_pubkey(keys.mur_home, None) else {
        return Vec::new();
    };
    let mut out = vec![current];
    let dir = keys
        .mur_home
        .join("agents")
        .join(crate::verify::CONTROL_PLANE_AGENT);
    if let Some(older) = head
        .key_version
        .and_then(|kv| crate::sign::resolve_writer_pubkey_strict(&dir, kv))
        .filter(|k| *k != current)
    {
        out.push(older);
    }
    out
}
