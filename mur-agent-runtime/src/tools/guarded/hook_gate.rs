//! #2175: the `pre_tool_use` gate hooks (B0 rules 1, 2, 4, 5) on the guarded
//! tool-call path. Run once per call, before the HITL gate, from the one
//! place both the agentic loop and MCP `tools/call` pass through.
//!
//! Semantics, fixed in the #2175 review:
//! - no chain wired → no rules → nothing here; a missing chain is not a refusal;
//! - `Allow` → the call takes the policy path it would have taken anyway;
//! - `Deny` / `Abort` → refused, code `hook_deny`;
//! - `Rewrite` → refused, code `hook_rewrite_unsupported` (nothing returns it
//!   today; running a rewritten input the gate never saw would be worse);
//! - a hook that fails → refused, code `hook_error`;
//! - `AskUser` → the call goes to a human through the ordinary approval
//!   batch, even when its policy is `Allow`.

use std::collections::HashMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::GuardedToolCall;
use crate::hitl::HitlDecision;
use crate::hooks::{Decision, HookChain, HookCtx, ToolCall};

pub(crate) const HOOK_DENY: &str = "hook_deny";
pub(crate) const HOOK_REWRITE_UNSUPPORTED: &str = "hook_rewrite_unsupported";
pub(crate) const HOOK_ERROR: &str = "hook_error";

/// The chain plus the context of the turn it gates. `ctx.turn_id` is the
/// runner's current turn, stamped when the `GuardedToolCall` is built.
#[derive(Clone)]
pub(crate) struct HookGate {
    pub(crate) chain: Arc<HookChain>,
    pub(crate) ctx: HookCtx,
    pub(crate) cancel: CancellationToken,
}

/// What the gate hooks said about one call, when it was not `Allow`.
pub(super) enum HookVerdict {
    Refuse(HitlDecision),
    Ask,
}

fn refusal(code: &'static str, reason: String) -> HookVerdict {
    HookVerdict::Refuse(HitlDecision {
        allow: false,
        reason: Some(reason),
        surface: None,
        awaiting_human: false,
        refusal_code: Some(code),
    })
}

impl GuardedToolCall {
    /// `call_id → verdict` for every call a gate hook did not allow.
    pub(super) async fn pre_tool_use(
        &self,
        task_id: &str,
        calls: &[crate::llm::ToolCallResult],
    ) -> HashMap<String, HookVerdict> {
        let mut out = HashMap::new();
        let Some(g) = &self.hooks else {
            return out;
        };
        for call in calls {
            let tc = ToolCall {
                tool_name: call.tool_name.clone(),
                mcp_server: None,
                call_id: call.call_id.clone(),
                input: call.input.clone(),
            };
            let verdict = match g.chain.pre_tool_use(&g.ctx, &tc, &g.cancel).await {
                Ok(Decision::Allow) => continue,
                Ok(Decision::Deny { reason }) => refusal(HOOK_DENY, reason),
                Ok(Decision::Abort) => refusal(HOOK_DENY, "aborted by a safety hook".into()),
                Ok(Decision::Rewrite(_)) => refusal(
                    HOOK_REWRITE_UNSUPPORTED,
                    "a safety hook asked to rewrite this call, which is not supported".into(),
                ),
                Ok(Decision::AskUser { .. }) => HookVerdict::Ask,
                Err(e) => refusal(HOOK_ERROR, format!("a safety hook failed: {e}")),
            };
            if let HookVerdict::Refuse(d) = &verdict {
                tracing::warn!(
                    task_id,
                    tool = %call.tool_name,
                    code = d.refusal_code.unwrap_or_default(),
                    reason = d.reason.as_deref().unwrap_or_default(),
                    "tool call refused by a pre_tool_use hook"
                );
            }
            out.insert(call.call_id.clone(), verdict);
        }
        out
    }
}
