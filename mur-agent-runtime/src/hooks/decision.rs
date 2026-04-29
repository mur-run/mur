//! `pre_tool_use` return type. `Clone` so the same decision can be consumed
//! by the chain dispatcher and recorded in telemetry.

use crate::hooks::types::AskDefault;
use mur_common::permissions::ScopeKey;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Decision {
    Allow,
    Deny {
        reason: String,
    },
    AskUser {
        prompt: String,
        default: AskDefault,
        scope_key: ScopeKey,
    },
    Rewrite(serde_json::Value),
    Abort,
}

impl Decision {
    pub fn is_allow(&self) -> bool {
        matches!(self, Decision::Allow)
    }
}
