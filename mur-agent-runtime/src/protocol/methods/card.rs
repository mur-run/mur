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
        if p.transport.stdio {
            transports.push("stdio");
        }
        if p.transport.socket.enabled && p.transport.socket.bind.starts_with("unix://") {
            transports.push("unix-socket");
        }
        Ok(json!({
            "protocolVersion": "a2a/0.3",
            "name": p.name,
            "id": p.id,
            "displayName": p.display_name,
            "version": p.version,
            "description": p.persona.description,
            "capabilities": p.capabilities,
            "transports": transports,
            "endpoints": {
                "stdio": "pipe://self",
                "unix-socket": p.transport.socket.bind,
            },
            "persona": {
                "category": p.persona.category,
                "traits": p.persona.traits,
            },
            "skills": p.skills.iter().map(|s| json!({"id": s})).collect::<Vec<_>>(),
            "entitlements": p.entitlements,
        }))
    }
}
