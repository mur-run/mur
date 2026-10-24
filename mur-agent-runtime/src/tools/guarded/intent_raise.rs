//! Layer 3 on gate B: which calls of one response the intent check raised.

use std::collections::HashMap;

use mur_common::hitl::RiskTier;

use super::GuardedToolCall;

impl GuardedToolCall {
    /// `call_id → raised tier` for every call the model put ABOVE its static
    /// tier. Calls it left alone (or could not answer for) are absent, so an
    /// empty map is exactly "layer 3 changed nothing". Asked concurrently:
    /// one response's calls should not pay the model's latency N times.
    pub(super) async fn intent_raises(
        &self,
        task_id: &str,
        calls: &[crate::llm::ToolCallResult],
    ) -> HashMap<String, RiskTier> {
        let Some(checker) = &self.intent else {
            return HashMap::new();
        };
        let rated = futures::future::join_all(calls.iter().map(|c| async move {
            let base = mur_common::hitl::tool_tier::classify(&c.tool_name, Some(&c.input));
            let tier = checker
                .tier_for(task_id, &c.tool_name, &c.input, base)
                .await;
            (tier > base).then(|| (c.call_id.clone(), tier))
        }))
        .await;
        rated.into_iter().flatten().collect()
    }
}
