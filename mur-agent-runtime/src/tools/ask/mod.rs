//! `ask` — the asker side of channel consult (design §D2, plan D1–D3).
//!
//! The asker's own runtime appends a self-signed `Ask` to its consult channel
//! (actor `Agent{self}`, fresh random nonce, deadline from `consult:` config),
//! then dials the answerer's `channel/consult(channel_id, nonce)` and returns
//! the answer. The dial carries no scope: the answerer derives it from this
//! agent's own profile.
//!
//! Readers of the `Ask` kind shipped in v2.99.0, one release before this
//! writer (§D7): a binary older than that reuses an `Ask`'s `seq` on its next
//! append.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use mur_channel::ChannelService;
use mur_common::channel::{AskPayload, ChannelActor, EventKind};
use mur_common::identity::AgentIdentity;
use serde_json::{Value, json};

use super::{ToolDef, ToolError, ToolExecutor, ToolOutput};

pub const ASK: &str = "ask";

/// Marker file in the agent home naming the channel this agent's `Ask`s go
/// to. Created on first use and remembered, like the schedule channel, so
/// every question an agent asks is in one findable place.
pub const ASK_CHANNEL_FILE: &str = "ask-channel";

/// Bytes of randomness in a nonce.
const NONCE_BYTES: usize = 16;

pub struct AskTool {
    pub mur_home: PathBuf,
    /// Canonical (on-disk) name of the asking agent.
    pub agent_name: String,
    /// Loaded before the sandbox sealed; signs the `Ask`.
    pub identity: Arc<AgentIdentity>,
    pub key_version: u32,
    /// `consult:` from the global config: how long an `Ask` stays answerable.
    pub consult: mur_common::config::ConsultConfig,
    /// The peers this agent may address (`communication.sends_to`, globs).
    pub sends_to: Vec<String>,
}

/// A fresh random nonce, hex.
fn fresh_nonce() -> String {
    use rand::RngCore;
    let mut b = [0u8; NONCE_BYTES];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The channel this agent's `Ask`s go to: the remembered one if it still
/// exists, else a new one, remembered. `to` joins it as a participant so the
/// Hub lists the thread under both agents.
fn ask_channel(
    svc: &ChannelService,
    mur_home: &Path,
    agent: &str,
    to: &str,
) -> anyhow::Result<String> {
    let marker = mur_home.join("agents").join(agent).join(ASK_CHANNEL_FILE);
    let id = match std::fs::read_to_string(&marker) {
        Ok(raw) if !raw.trim().is_empty() && svc.exists(raw.trim()) => raw.trim().to_string(),
        _ => {
            let ch = svc.create_for_agent(agent)?;
            std::fs::write(&marker, &ch.id).context("record the ask channel id")?;
            ch.id
        }
    };
    svc.add_participant(&id, to, mur_common::channel::ParticipantRole::Delegate)?;
    Ok(id)
}

impl AskTool {
    /// D1: append the signed `Ask`. Returns `(channel_id, nonce)`.
    pub fn append_ask(&self, to: &str, question: &str) -> anyhow::Result<(String, String)> {
        let svc = ChannelService::open(&self.mur_home)?;
        let channel_id = ask_channel(&svc, &self.mur_home, &self.agent_name, to)?;
        let nonce = fresh_nonce();
        let now = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0);
        let expires_at_ms = i64::try_from(self.consult.ask_expires_at_ms(now)).unwrap_or(i64::MAX);
        let payload = AskPayload {
            to: to.to_string(),
            question: question.to_string(),
            expires_at_ms,
            nonce: nonce.clone(),
        };
        svc.append_signed(
            &channel_id,
            &self.identity,
            self.key_version,
            ChannelActor::Agent {
                id: self.agent_name.clone(),
            },
            EventKind::Ask,
            serde_json::to_value(&payload)?,
            Some(nonce.clone()),
        )?;
        Ok((channel_id, nonce))
    }
}

#[async_trait::async_trait]
impl ToolExecutor for AskTool {
    fn name(&self) -> &str {
        ASK
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: ASK.into(),
            description: "Ask another MUR agent a question and wait for its answer. The question \
is posted to a channel as a signed Ask; the other agent answers in a restricted turn that can \
only read files BOTH of you may read, using read_file alone. The answer is posted to the same \
channel, where other participants can see it. Use it for knowledge the other agent has, not \
for work you want done."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "to": {
                        "type": "string",
                        "description": "The agent to ask, by name"
                    },
                    "question": {
                        "type": "string",
                        "description": "The question, self-contained: the other agent sees nothing else"
                    }
                },
                "required": ["to", "question"]
            }),
        }
    }

    async fn execute(&self, input: Value) -> Result<ToolOutput, ToolError> {
        let field = |k: &str| {
            input
                .get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .ok_or_else(|| ToolError::InvalidInput(format!("`{k}` is required")))
        };
        let to = field("to")?;
        let question = field("question")?;
        if to == self.agent_name {
            return Err(ToolError::InvalidInput("an agent cannot ask itself".into()));
        }
        if !crate::communication_policy::sends_to_allows(&self.sends_to, &to) {
            return Err(ToolError::NotAuthorized(format!(
                "this agent may not address '{to}': it is not in communication.sends_to"
            )));
        }
        // Resolve the peer before writing anything, so an Ask nobody can
        // answer is never posted.
        let sock = crate::protocol::peer_dial::peer_socket(
            &self.mur_home,
            &to,
            mur_common::build::CHANNEL_CONSULT_METHOD,
        )
        .map_err(|e| ToolError::Execution(e.to_string()))?;
        let (channel_id, nonce) = self
            .append_ask(&to, &question)
            .map_err(|e| ToolError::Execution(format!("could not post the Ask: {e}")))?;
        // D2.
        let out = crate::protocol::peer_dial::call(
            &sock,
            mur_common::build::CHANNEL_CONSULT_METHOD,
            json!({ "channel_id": channel_id, "nonce": nonce }),
            crate::protocol::peer_dial::PEER_IDLE_TIMEOUT,
        )
        .await
        .map_err(|e| ToolError::Execution(format!("{to} did not answer: {e}")))?;
        let text = out.get("text").and_then(Value::as_str).unwrap_or_default();
        Ok(format!("{to} answered:\n{text}").into())
    }
}

#[cfg(test)]
mod tests;
