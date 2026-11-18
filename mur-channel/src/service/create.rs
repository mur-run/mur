//! Channel creators and the owner they are created with (B3, D-2, D-3).
//!
//! [`ChannelOwner::Human`] writes the manifest (already `v: 3`, D1) and then
//! a seq-0 `owner-key-enrolled` record (`human_cut: None`) signed by the
//! human, its head sealed by the router in the same append lock. A crash
//! between the two leaves a v3 channel with no record: legacy-shaped and
//! never trusted, which C1 enrolls on the next human write.
//! [`ChannelOwner::Legacy`] is today's behaviour (every runtime caller).

use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use mur_common::channel::{
    CHANNEL_SCHEMA_VERSION, Channel, ChannelActor, ChannelPurpose, ChannelState, EventKind, Goal,
    OwnerKeyEnrolledPayload, Participant, ParticipantRole,
};
use mur_common::identity::{AgentIdentity, encode_pubkey_bytes};

use super::ChannelService;
use super::anchored::{HeadSealer, HeadSigner};

/// The human owner's signing key, as the channel layer sees it. Implemented
/// outside the sandbox only (mur-core's `HumanSigner`): agents cannot read
/// `keys/.human/`.
pub trait HumanSign {
    fn pubkey(&self) -> [u8; 32];
    /// The human rotation chain's head version.
    fn key_version(&self) -> u32;
    fn sign(&self, msg: &[u8]) -> [u8; 64];
}

/// The router key that seals every head of an owned channel (H1).
#[derive(Clone, Copy)]
pub struct RouterSeal<'a> {
    pub identity: &'a AgentIdentity,
    pub kv: u32,
    /// The router agent's home (its `rotations.jsonl`).
    pub key_home: &'a Path,
}

/// Who a new channel is created for.
#[derive(Clone, Copy)]
pub enum ChannelOwner<'a> {
    /// No owner record (runtime callers, and every channel before phase 2).
    Legacy,
    /// The human owns it from seq 0.
    Human {
        signer: &'a dyn HumanSign,
        router: RouterSeal<'a>,
    },
}

impl ChannelService {
    /// Create a fresh channel whose participants are the local human (owner)
    /// and one agent (delegate). Used by both CLI and Hub when opening a chat.
    pub fn create_for_agent(&self, agent: &str, owner: ChannelOwner<'_>) -> Result<Channel> {
        let now = Utc::now();
        let ch = Channel {
            v: CHANNEL_SCHEMA_VERSION,
            id: uuid::Uuid::now_v7().to_string(),
            title: String::new(),
            goal: Goal::default(),
            state: ChannelState::Working,
            purpose: Some(ChannelPurpose::Conversation),
            owner: ChannelActor::local_human(),
            participants: vec![
                Participant {
                    actor: ChannelActor::local_human(),
                    role: ParticipantRole::Owner,
                    joined_at: now,
                },
                Participant {
                    actor: ChannelActor::Agent {
                        id: agent.to_string(),
                    },
                    role: ParticipantRole::Delegate,
                    joined_at: now,
                },
            ],
            created_at: now,
            updated_at: now,
        };
        self.create_owned(ch, owner)
    }

    /// Create the long-lived shared channel for a fleet. Id is the stable,
    /// filesystem-safe `fleet-<name>`. Router gets `Router`, members `Delegate`.
    pub fn create_for_fleet(
        &self,
        fleet_name: &str,
        router: &str,
        members: &[String],
        owner: ChannelOwner<'_>,
    ) -> Result<Channel> {
        let now = Utc::now();
        let mut participants = vec![
            Participant {
                actor: ChannelActor::local_human(),
                role: ParticipantRole::Owner,
                joined_at: now,
            },
            Participant {
                actor: ChannelActor::Agent {
                    id: router.to_string(),
                },
                role: ParticipantRole::Router,
                joined_at: now,
            },
        ];
        for m in members {
            participants.push(Participant {
                actor: ChannelActor::Agent { id: m.clone() },
                role: ParticipantRole::Delegate,
                joined_at: now,
            });
        }
        let ch = Channel {
            v: CHANNEL_SCHEMA_VERSION,
            id: format!("fleet-{fleet_name}"),
            title: format!("fleet: {fleet_name}"),
            goal: Goal::default(),
            state: ChannelState::Working,
            purpose: Some(ChannelPurpose::FleetRun),
            owner: ChannelActor::local_human(),
            participants,
            created_at: now,
            updated_at: now,
        };
        self.create_owned(ch, owner)
    }

    /// Create a channel that records a workflow execution. No agent participant;
    /// the DAG executor acts as `ChannelActor::System`.
    pub fn create_for_workflow(
        &self,
        skill_name: &str,
        owner: ChannelOwner<'_>,
    ) -> Result<Channel> {
        self.create_for_workflow_with_id(skill_name, &uuid::Uuid::now_v7().to_string(), owner)
    }

    /// [`Self::create_for_workflow`] under an id the caller minted earlier —
    /// a sealed agent that already handed the id back as its run handle
    /// (#2379). Refuses an id that is not a UUID or is already taken, so a
    /// replayed request can never append to someone else's channel.
    pub fn create_for_workflow_with_id(
        &self,
        skill_name: &str,
        id: &str,
        owner: ChannelOwner<'_>,
    ) -> Result<Channel> {
        uuid::Uuid::parse_str(id).map_err(|e| anyhow::anyhow!("channel id {id:?}: {e}"))?;
        anyhow::ensure!(!self.exists(id), "channel {id} already exists");
        let now = Utc::now();
        let ch = Channel {
            v: CHANNEL_SCHEMA_VERSION,
            id: id.to_string(),
            title: format!("workflow: {skill_name}"),
            goal: Goal::default(),
            state: ChannelState::Working,
            purpose: Some(ChannelPurpose::WorkflowRun),
            owner: ChannelActor::local_human(),
            participants: vec![],
            created_at: now,
            updated_at: now,
        };
        self.create_owned(ch, owner)
    }

    /// Persist `ch`, then (for a human owner) its seq-0 owner record.
    fn create_owned(&self, ch: Channel, owner: ChannelOwner<'_>) -> Result<Channel> {
        self.store.create(&ch)?;
        self.index.upsert(&ch)?;
        if let ChannelOwner::Human { signer, router } = owner {
            self.enroll_at_creation(&ch.id, signer, router)
                .with_context(|| format!("record the human owner of channel {}", ch.id))?;
        }
        Ok(ch)
    }

    /// Append the seq-0 `owner-key-enrolled` record, signed by the human
    /// under the append lock and anchored by a router-sealed head.
    fn enroll_at_creation(
        &self,
        channel_id: &str,
        signer: &dyn HumanSign,
        router: RouterSeal<'_>,
    ) -> Result<()> {
        let owner_key = encode_pubkey_bytes(&signer.pubkey());
        let kv = signer.key_version();
        let payload = serde_json::to_value(OwnerKeyEnrolledPayload {
            owner_key: owner_key.clone(),
            key_version: kv,
            human_cut: None,
        })?;
        let head = HeadSigner {
            identity: router.identity,
            kv: router.kv,
            key_home: router.key_home,
            sealer: HeadSealer::Router {
                identity: router.identity,
                kv: router.kv,
                key_home: router.key_home,
                live_record: true,
            },
        };
        let ev = self.with_hooks(Some(head), |hooks| {
            self.store.append_signed_by_with(
                channel_id,
                kv,
                &|msg| signer.sign(msg),
                ChannelActor::local_human_with_key(owner_key),
                EventKind::OwnerKeyEnrolled,
                payload,
                None,
                hooks,
            )
        })?;
        self.after_write(channel_id, &ev, None);
        Ok(())
    }
}

#[cfg(test)]
#[path = "create_tests.rs"]
mod tests;
