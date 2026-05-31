use async_trait::async_trait;
use mur_common::guard::TrashGuardLogic;
use tokio_util::sync::CancellationToken;

use crate::hooks::{AskDefault, Decision, Hook, HookCtx, HookError, ToolCall};

pub struct TrashGuard {
    pub logic: TrashGuardLogic,
}

impl TrashGuard {
    pub fn new(logic: TrashGuardLogic) -> Self {
        Self { logic }
    }
}

#[async_trait]
impl Hook for TrashGuard {
    fn name(&self) -> &str {
        "TrashGuard"
    }

    async fn pre_tool_use(
        &self,
        ctx: &HookCtx,
        call: &ToolCall,
        _tok: &CancellationToken,
    ) -> Result<Decision, HookError> {
        let detections = self.logic.detect(&call.tool_name, &call.input);

        if detections.is_empty() {
            return Ok(Decision::Allow);
        }

        // Schema-level enforcement
        let path_count = detections
            .iter()
            .map(|d| match d {
                mur_common::guard::DestructivePattern::Rm { paths, .. } => paths.len(),
                mur_common::guard::DestructivePattern::PythonRemove { paths, .. } => paths.len(),
                mur_common::guard::DestructivePattern::McpDelete { paths, .. } => paths.len(),
                mur_common::guard::DestructivePattern::A2ADelete { paths } => paths.len(),
            })
            .sum::<usize>();

        // Batch size check
        if let Err(e) = self.logic.check_batch_size(path_count) {
            return Ok(Decision::Deny {
                reason: e.to_string(),
            });
        }

        // Wildcard check
        if detections.iter().any(|d| d.contains_wildcard()) {
            return Ok(Decision::Deny {
                reason: "wildcard patterns rejected in destructive operations".into(),
            });
        }

        // Two-phase: first destructive op → AskUser (default Deny)
        let scope_key = mur_common::permissions::ScopeKey {
            agent_id: ctx.agent_uuid.clone(),
            tool_name: format!("trash_guard::{}", call.name()),
            input_schema_hash: String::new(),
        };
        Ok(Decision::AskUser {
            prompt: format!(
                "Agent wants to perform a destructive operation ({} paths). \
                 Moving to Trash with {}-minute cancel window. Allow?",
                path_count,
                self.logic.config().cancel_window_minutes
            ),
            default: AskDefault::Deny,
            scope_key,
        })
    }
}
