//! Repairing Gate B's decision channel (an agent's `hitl-channel`, #1965).
//!
//! Gate B does not trust `channel-heads/`; it checks the log against a head
//! sealed by the agent's own key in `<agent_home>/hitl-heads/`
//! (`mur_agent_runtime::hitl::integrity`). So for this channel, damage means
//! either check failing, and the fresh chain must open with a head sealed
//! by that same key — otherwise the runtime would read "records but no
//! signed head" and the channel would stay broken.
//!
//! The repair event is therefore written exactly the way the runtime appends
//! its own decisions: signed by the agent key, through the runtime's own seal
//! hook, which seals a genesis head over the one-line log. This runs in the
//! operator's `mur` process outside the agent sandbox, the same place
//! `mur agent rekey` reads that key from; a process that cannot read it is
//! refused before anything is touched.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use mur_agent_runtime::hitl::integrity;
use mur_channel::ChannelService;
use mur_channel::service::restart::RestartSigner;
use mur_common::channel::ChannelActor;
use mur_common::identity::AgentIdentity;

/// The agent that owns a Gate B decision channel, with the key that seals it.
pub(super) struct GateB {
    pub agent: String,
    identity: Arc<AgentIdentity>,
    key_version: u32,
}

impl GateB {
    /// Load the owning agent's key. Fails (touching nothing) when this
    /// process cannot read it.
    pub fn load(mur_home: &Path, agent: String) -> Result<Self> {
        let agent_home = mur_home.join("agents").join(&agent);
        let identity = AgentIdentity::load(&agent_home).with_context(|| {
            format!(
                "channel is agent `{agent}`'s HITL decision channel; repairing it needs that \
                 agent's signing key, which this process cannot read"
            )
        })?;
        Ok(Self {
            key_version: crate::channel_writer::read_key_version(&agent_home),
            agent,
            identity: Arc::new(identity),
        })
    }

    /// Why Gate B would refuse this log, or `None` when its seal holds.
    pub fn damage(&self, mur_home: &Path, channel_id: &str) -> Option<String> {
        let _guard = integrity::read_lock(mur_home, channel_id);
        integrity::verify(
            mur_home,
            &self.agent,
            channel_id,
            &self.identity.verifying_key_bytes(),
        )
        .err()
        .map(|e| e.0)
    }

    /// The agent-sealed head, if one can be read (for the repair record).
    pub fn old_head(&self, mur_home: &Path, channel_id: &str) -> Option<serde_json::Value> {
        let head = mur_channel::head_io::read_head(
            &integrity::heads_dir(mur_home, &self.agent),
            channel_id,
        )
        .ok()
        .flatten()?;
        Some(serde_json::json!({
            "lines": head.lines,
            "seq": head.seq,
            "chain_hash": head.chain_hash,
        }))
    }

    /// Drop the agent-sealed head. Called under the append lock, after the
    /// log was moved aside.
    pub fn drop_head(&self, mur_home: &Path, channel_id: &str) -> Result<()> {
        mur_channel::head_io::remove_head(&integrity::heads_dir(mur_home, &self.agent), channel_id)
    }

    /// A service whose seal hook is the runtime's own, so the fresh chain's
    /// genesis head in `hitl-heads/` is written exactly as the runtime would.
    pub fn service(&self, mur_home: &Path) -> Result<ChannelService> {
        Ok(
            ChannelService::open(mur_home)?.with_seal(integrity::seal_hook(
                mur_home.to_path_buf(),
                self.agent.clone(),
                self.identity.clone(),
                self.key_version,
            )),
        )
    }

    /// The agent key signs the fresh chain; the router head is not sealed
    /// (Gate B reads only its own).
    pub fn signer(&self) -> RestartSigner<'_> {
        RestartSigner {
            identity: &self.identity,
            kv: self.key_version,
            anchor_home: None,
        }
    }

    pub fn actor(&self) -> ChannelActor {
        ChannelActor::Agent {
            id: self.agent.clone(),
        }
    }
}
