//! A2A method: `memory/reload` — rebuild the live skill/memory set from disk.
//!
//! The out-of-process half of the one reload mechanism. `remember` calls
//! [`RuntimeSkills::reload`] directly because it runs inside this process;
//! murmur's `/remember` and `/forget` run in the CLI process and dial this
//! instead. Same function, two callers — deliberately not two reload paths,
//! which is how the two halves of a fact start disagreeing.
//!
//! Modelled on `model/set`: the CLI changes state on disk, then tells the
//! running agent, rather than either side polling or watching.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::protocol::a2a_server::{HandlerError, MethodHandler, RequestContext};
use crate::skills::RuntimeSkills;

pub struct MemoryReloadHandler {
    skills: Arc<RuntimeSkills>,
}

impl MemoryReloadHandler {
    pub fn new(skills: Arc<RuntimeSkills>) -> Self {
        Self { skills }
    }
}

#[async_trait]
impl MethodHandler for MemoryReloadHandler {
    async fn handle(
        &self,
        _params: Option<Value>,
        _ctx: &RequestContext,
    ) -> Result<Value, HandlerError> {
        let count = self
            .skills
            .reload()
            .map_err(|e| HandlerError::Internal(format!("reload skills: {e:#}")))?;
        tracing::info!(count, "memory/reload: live skill set rebuilt");
        Ok(json!({ "skills": count, "effective": "next-turn" }))
    }
}
