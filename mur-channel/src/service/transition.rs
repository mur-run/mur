//! State transitions: the unsigned, signed and anchored entry points, all
//! funnelled through [`ChannelService::write_event`].

use anyhow::Result;
use mur_common::channel::{ChannelActor, ChannelEvent, ChannelState, EventKind};

use super::ChannelService;
use super::anchored::HeadSigner;
use super::state_str;

impl ChannelService {
    /// Emit a `StateChange` event and persist the new state on the manifest.
    /// `run_id` stamps the run-status run boundary on the payload; pass `None`
    /// for transitions that are not part of a recorded run.
    /// Unsigned state transition. Prefer `transition_signed` from anywhere a
    /// writer identity is available — see that method for why.
    pub fn transition(
        &self,
        channel_id: &str,
        new_state: ChannelState,
        actor: ChannelActor,
        run_id: Option<&str>,
    ) -> Result<ChannelEvent> {
        self.transition_signed(channel_id, new_state, actor, run_id, None)
    }

    /// A state transition SIGNED by the channel's writer, when one is given.
    ///
    /// `transition` used to be the only entry point and never signed, so every
    /// `StateChange` on every channel was unsigned regardless of who wrote it
    /// — 33 of 33 on one live fleet channel, from sandboxed and unsandboxed
    /// processes alike. `Message` and `Delegation` events went through the
    /// signing path and these did not, which makes "the run started" and "the
    /// run failed" the two events in a channel that nothing can attribute.
    pub fn transition_signed(
        &self,
        channel_id: &str,
        new_state: ChannelState,
        actor: ChannelActor,
        run_id: Option<&str>,
        signer: Option<(&mur_common::identity::AgentIdentity, u32)>,
    ) -> Result<ChannelEvent> {
        self.transition_with(channel_id, new_state, actor, run_id, signer, None)
    }

    /// [`transition_signed`](Self::transition_signed) that also advances the
    /// signed channel head under the same lock, like
    /// [`append_and_anchor`](Self::append_and_anchor). Without it a
    /// transition written after an anchored append leaves the head one line
    /// behind the log, with nothing marking the lag.
    ///
    /// **Only call from outside the agent sandbox**, for the same reason as
    /// `append_and_anchor`.
    #[allow(clippy::too_many_arguments)]
    pub fn transition_and_anchor(
        &self,
        channel_id: &str,
        new_state: ChannelState,
        actor: ChannelActor,
        run_id: Option<&str>,
        identity: &mur_common::identity::AgentIdentity,
        kv: u32,
        key_home: &std::path::Path,
    ) -> Result<ChannelEvent> {
        self.transition_with(
            channel_id,
            new_state,
            actor,
            run_id,
            Some((identity, kv)),
            Some(key_home),
        )
    }

    fn transition_with(
        &self,
        channel_id: &str,
        new_state: ChannelState,
        actor: ChannelActor,
        run_id: Option<&str>,
        signer: Option<(&mur_common::identity::AgentIdentity, u32)>,
        anchored: Option<&std::path::Path>,
    ) -> Result<ChannelEvent> {
        let old_state = self
            .store
            .load_manifest(channel_id)
            .map(|ch| ch.state)
            .unwrap_or(ChannelState::Working);
        let mut payload = serde_json::json!({
            "from": state_str(old_state),
            "to":   state_str(new_state),
        });
        if let Some(run_id) = run_id {
            payload["run_id"] = serde_json::json!(run_id);
        }
        // Same canonical sign-input as `append_signed`: no idempotency key
        // participates here because a transition never carries one.
        let (sig, kv) = match signer {
            Some((id, kv)) => (
                Some(crate::sign::sign_event(
                    id,
                    channel_id,
                    &actor,
                    EventKind::StateChange,
                    &payload,
                    None,
                )),
                Some(kv),
            ),
            None => (None, None),
        };
        let head_signer = match (signer, anchored) {
            (Some((identity, kv)), Some(key_home)) => Some(HeadSigner {
                identity,
                kv,
                key_home,
            }),
            _ => None,
        };
        self.write_event(
            channel_id,
            actor,
            EventKind::StateChange,
            payload,
            None,
            sig,
            kv,
            Some(new_state),
            head_signer,
        )
    }
}
