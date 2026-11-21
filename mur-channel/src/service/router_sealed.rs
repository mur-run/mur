//! An anchored write by an agent that signs its own line on a channel whose
//! head belongs to the control plane.
//!
//! A fleet's own router (`router: orchestrator`) speaks as itself, so its line
//! carries its signature. Every other line on that channel is the control
//! plane's, and so is the head: a head the fleet router sealed is one the
//! control plane cannot advance, and the reverse (#2081 finding C).

use std::path::Path;

use anyhow::Result;
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
use mur_common::identity::AgentIdentity;

use super::ChannelService;
use super::anchored::{HeadSealer, HeadSigner};
use super::create::RouterSeal;

impl ChannelService {
    /// [`append_and_anchor`](Self::append_and_anchor) where `identity` only
    /// signs the event and `router` seals the head, owned channel or not.
    ///
    /// A head `identity` sealed earlier is taken over once, unless the
    /// channel has a live owner record. Same sandbox caveat and
    /// [`AnchorLagError`](crate::anchor::AnchorLagError) contract.
    #[allow(clippy::too_many_arguments)]
    pub fn append_and_anchor_router_sealed(
        &self,
        channel_id: &str,
        identity: &AgentIdentity,
        kv: u32,
        key_home: &Path,
        router: RouterSeal<'_>,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
    ) -> Result<ChannelEvent> {
        let signer = HeadSigner {
            sealer: HeadSealer::RouterAlways { router },
            ..HeadSigner::event(identity, kv, key_home)
        };
        let ev = self.with_hooks(Some(signer), |hooks| {
            self.store.append_signed_event_with(
                channel_id,
                identity,
                kv,
                actor,
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
