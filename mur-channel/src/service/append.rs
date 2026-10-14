use super::*;

impl ChannelService {
    /// Append an event with an arbitrary payload, bumping `updated_at` + index.
    pub fn append(
        &self,
        channel_id: &str,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
    ) -> Result<ChannelEvent> {
        self.write_event(
            channel_id,
            actor,
            kind,
            payload,
            idempotency_key,
            None,
            None,
            None,
            None,
        )
    }

    /// Sign an event with `identity` (key_version `kv`) and append it. Used by
    /// the channel's writer (the router/owner) so the log is forgery-resistant.
    #[allow(clippy::too_many_arguments)]
    pub fn append_signed(
        &self,
        channel_id: &str,
        identity: &mur_common::identity::AgentIdentity,
        kv: u32,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
    ) -> Result<ChannelEvent> {
        self.append_signed_with(
            channel_id,
            identity,
            kv,
            actor,
            kind,
            payload,
            idempotency_key,
            None,
        )
    }

    /// [`Self::append_signed`] with a per-call seal hook, for a caller whose
    /// hook depends on the writer's key (known only at call time).
    ///
    /// `Some(hook)` seals with that hook *instead of* the service's own (one
    /// seal per event); `None` falls back to the service hook, so a service
    /// built `with_seal` never silently skips sealing. Signing, sealing and
    /// the read-model refresh stay in this one place.
    #[allow(clippy::too_many_arguments)]
    pub fn append_signed_with(
        &self,
        channel_id: &str,
        identity: &mur_common::identity::AgentIdentity,
        kv: u32,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
        seal: Option<&crate::store::SealHook<'_>>,
    ) -> Result<ChannelEvent> {
        let ev = self.store.append_signed_event_with(
            channel_id,
            identity,
            kv,
            actor,
            kind,
            payload,
            idempotency_key,
            seal.or_else(|| {
                self.seal
                    .as_deref()
                    .map(|h| h as &crate::store::SealHook<'_>)
            }),
        )?;
        if let Ok(mut ch) = self.store.load_manifest(channel_id) {
            ch.updated_at = ev.ts;
            if let Some(t) = Self::derived_title(&ch, &ev) {
                ch.title = t;
            }
            self.refresh_read_model(&ch, &ev);
        }
        Ok(ev)
    }

    /// Sign an event against sig-input **v1** (no `prev_hash`) and append it.
    ///
    /// Only for writers whose events are verified by `verify_event_sig`, which
    /// is v1-only on purpose: commander directives. A directive's signature
    /// proves "who authorised what"; chain position is the router chain's job.
    /// The line still feeds the next v2 line's `prev_hash`, so a mid-log
    /// deletion of it is caught by chain verification. Do not route this
    /// through `append_signed` (v2) — `fold_governance` would silently drop it.
    #[allow(clippy::too_many_arguments)]
    pub fn append_signed_v1(
        &self,
        channel_id: &str,
        identity: &mur_common::identity::AgentIdentity,
        kv: u32,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
    ) -> Result<ChannelEvent> {
        let sig = crate::sign::sign_event(
            identity,
            channel_id,
            &actor,
            kind,
            &payload,
            idempotency_key.as_deref(),
        );
        self.write_event(
            channel_id,
            actor,
            kind,
            payload,
            idempotency_key,
            Some(sig),
            Some(kv),
            None,
            None,
        )
    }

    /// Append an event whose signature was produced elsewhere (e.g. a
    /// Commander governance directive signed with the Commander's key).
    ///
    /// The service does NOT verify `sig`: it cannot attribute it to a key.
    /// Verification stays with the caller, which must reject a bad signature
    /// *before* calling this. What the service adds is the same write path as
    /// every other append: the seal hook runs and the read model is refreshed,
    /// so the event is visible in conversation lists and ordering.
    #[allow(clippy::too_many_arguments)]
    pub fn append_presigned(
        &self,
        channel_id: &str,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
        sig: String,
        key_version: Option<u32>,
    ) -> Result<ChannelEvent> {
        self.write_event(
            channel_id,
            actor,
            kind,
            payload,
            idempotency_key,
            Some(sig),
            key_version,
            None,
            None,
        )
    }

    /// Append a `Delegation` event (actor `System`) recording that `target_agent`
    /// was handed the sub-goal under `child_task_id`. `idempotency_key` is set by
    /// the caller (deterministic in v3b) but NOT yet de-duplicated (v3c).
    /// `run_id` stamps the run-status run boundary on the payload (see
    /// [`delegation_payload`]); pass `None` for delegations outside any run.
    pub fn append_delegation(
        &self,
        channel_id: &str,
        target_agent: &str,
        child_task_id: &str,
        idempotency_key: Option<String>,
        run_id: Option<&str>,
    ) -> Result<ChannelEvent> {
        let payload = delegation_payload(channel_id, target_agent, child_task_id, None, run_id);
        self.append(
            channel_id,
            ChannelActor::System,
            EventKind::Delegation,
            payload,
            idempotency_key,
        )
    }

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
        self.transition_signed_with(channel_id, new_state, actor, run_id, signer, None)
    }

    /// [`Self::transition_signed`] with a per-call seal hook, the transition
    /// twin of [`Self::append_signed_with`].
    pub fn transition_signed_with(
        &self,
        channel_id: &str,
        new_state: ChannelState,
        actor: ChannelActor,
        run_id: Option<&str>,
        signer: Option<(&mur_common::identity::AgentIdentity, u32)>,
        seal: Option<&crate::store::SealHook<'_>>,
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
        self.write_event(
            channel_id,
            actor,
            EventKind::StateChange,
            payload,
            None,
            sig,
            kv,
            Some(new_state),
            seal,
        )
    }
}
