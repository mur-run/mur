//! agent/card method — project AgentProfile into an A2A Agent Card.

use crate::profile::Profile;
use crate::protocol::a2a_server::{HandlerError, MethodHandler};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

pub struct CardHandler {
    profile: Arc<Profile>,
}

impl CardHandler {
    pub fn new(profile: Arc<Profile>) -> Self {
        Self { profile }
    }
}

#[async_trait]
impl MethodHandler for CardHandler {
    async fn handle(&self, _params: Option<Value>) -> Result<Value, HandlerError> {
        let p = &self.profile.inner;

        let mut transports: Vec<&str> = vec![];
        let mut endpoints: Vec<Value> = vec![];

        if p.transport.tcp.enabled && !p.transport.tcp.bind.is_empty() {
            transports.push("tcp+noise");
            endpoints.push(json!({
                "transport": "tcp+noise",
                "url": format!("tcp://{}", p.transport.tcp.bind),
                "reachability": "lan",
            }));
        }
        if p.transport.socket.enabled && p.transport.socket.bind.starts_with("unix://") {
            transports.push("unix-socket");
            endpoints.push(json!({
                "transport": "unix-socket",
                "url": p.transport.socket.bind,
                "reachability": "local",
            }));
        }
        if p.transport.stdio {
            transports.push("stdio");
            endpoints.push(json!({
                "transport": "stdio",
                "url": "pipe://self",
                "reachability": "local",
            }));
        }

        // M3.1: include previous_pubkey + grace_expires_at when within grace.
        // Peers use these to retry handshakes against either key during the
        // grace window.
        let mut card = json!({
            "protocolVersion": "a2a/0.3",
            "name": p.name,
            "id": p.id,
            "pubkey": p.identity.pubkey,
            "algorithm": p.identity.algorithm,
            "key_version": p.identity.key_version,
            "displayName": p.display_name,
            "version": p.version,
            "description": p.persona.description,
            "capabilities": p.capabilities,
            "transports": transports,
            "endpoints": endpoints,
            "deployment": {
                "type": p.deployment.deployment_type,
                "region": p.deployment.region,
                "environment": p.deployment.environment,
            },
            "persona": {
                "category": p.persona.category,
                "traits": p.persona.traits,
            },
            "skills": p.skills.iter().map(|s| json!({"id": s})).collect::<Vec<_>>(),
            "entitlements": p.entitlements,
        });

        if let (Some(prev), Some(expires)) =
            (&p.identity.previous_pubkey, &p.identity.grace_expires_at)
        {
            // Only advertise the previous key if grace is still active. We
            // parse the timestamp lazily; if it's malformed we err on the
            // side of *not* publishing it.
            let still_in_grace = chrono::DateTime::parse_from_rfc3339(expires)
                .map(|exp| exp > chrono::Utc::now())
                .unwrap_or(false);
            if still_in_grace {
                card["previous_pubkey"] = json!(prev);
                card["previous_key_version"] = json!(p.identity.previous_key_version);
                card["grace_expires_at"] = json!(expires);
            }
        }

        Ok(card)
    }
}
