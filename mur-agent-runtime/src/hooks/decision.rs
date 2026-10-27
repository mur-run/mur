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
        /// #2175: `true` = the question is about THIS turn (B0 rule 4: "this
        /// attachment may carry instructions — run anyway?"), so the decision
        /// store neither answers it nor records the answer. `false` = a
        /// reusable permission (rules 1 and 2), remembered as usual.
        #[serde(default)]
        one_shot: bool,
    },
    Rewrite(serde_json::Value),
    Abort,
}

impl Decision {
    pub fn is_allow(&self) -> bool {
        matches!(self, Decision::Allow)
    }
}
