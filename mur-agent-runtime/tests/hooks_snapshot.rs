//! End-to-end hook fire-sequence snapshot.
//!
//! Exercises every method of `HookChain` against a 4-handler chain and
//! asserts the recorded fire sequence matches the golden snapshot.
//! The fixture mirrors the canonical "Telegram inbound → user agent
//! task_runner → outbound MCP → reply" path called out in roadmap §3.2.

use std::sync::Arc;
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;

use mur_agent_runtime::hooks::{
    A2AEnvelopeView, Decision, Hook, HookChain, HookCtx, HookError, MessagePatch, OutboundView,
    Phase, PromptPatch, PromptView, ShutdownReason, Step, ToolCall, ToolResult, TriggerKind,
    TriggerPayload,
};
use mur_common::AgentProfile;

#[derive(Default)]
struct Recording {
    events: Mutex<Vec<String>>,
}

impl Recording {
    fn push(&self, s: String) {
        self.events.lock().unwrap().push(s);
    }
    fn snapshot(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }
}

struct RecordHook {
    name: String,
    rec: Arc<Recording>,
}

#[async_trait::async_trait]
impl Hook for RecordHook {
    fn name(&self) -> &str {
        &self.name
    }
    async fn on_startup(
        &self,
        _: &HookCtx,
        _: &AgentProfile,
        _: &CancellationToken,
    ) -> Result<(), HookError> {
        self.rec.push(format!("{}:startup", self.name));
        Ok(())
    }
    async fn on_trigger_fired(
        &self,
        _: &HookCtx,
        kind: TriggerKind,
        _: &TriggerPayload,
        _: &CancellationToken,
    ) -> Result<(), HookError> {
        self.rec.push(format!("{}:trigger:{:?}", self.name, kind));
        Ok(())
    }
    async fn on_message_received(
        &self,
        _: &HookCtx,
        e: &A2AEnvelopeView,
        _: &CancellationToken,
    ) -> Result<(), HookError> {
        self.rec
            .push(format!("{}:msg_recv:{}", self.name, e.method));
        Ok(())
    }
    async fn on_prompt_submit(
        &self,
        _: &HookCtx,
        _: &PromptView,
        _: &CancellationToken,
    ) -> Result<PromptPatch, HookError> {
        self.rec.push(format!("{}:prompt", self.name));
        Ok(PromptPatch::noop())
    }
    async fn pre_tool_use(
        &self,
        _: &HookCtx,
        c: &ToolCall,
        _: &CancellationToken,
    ) -> Result<Decision, HookError> {
        self.rec
            .push(format!("{}:pre_tool:{}", self.name, c.tool_name));
        Ok(Decision::Allow)
    }
    async fn post_tool_use(
        &self,
        _: &HookCtx,
        c: &ToolCall,
        _: &ToolResult,
        _: &CancellationToken,
    ) -> Result<(), HookError> {
        self.rec
            .push(format!("{}:post_tool:{}", self.name, c.tool_name));
        Ok(())
    }
    async fn on_step_finish(
        &self,
        _: &HookCtx,
        _: &Step,
        _: &CancellationToken,
    ) -> Result<(), HookError> {
        self.rec.push(format!("{}:step", self.name));
        Ok(())
    }
    async fn on_message_send(
        &self,
        _: &HookCtx,
        _: &OutboundView,
        _: &CancellationToken,
    ) -> Result<MessagePatch, HookError> {
        self.rec.push(format!("{}:msg_send", self.name));
        Ok(MessagePatch::noop())
    }
    async fn on_error(
        &self,
        _: &HookCtx,
        _: &HookError,
        p: Phase,
        _: &CancellationToken,
    ) -> Result<(), HookError> {
        self.rec.push(format!("{}:error:{:?}", self.name, p));
        Ok(())
    }
    async fn on_shutdown(
        &self,
        _: &HookCtx,
        r: ShutdownReason,
        _: &CancellationToken,
    ) -> Result<(), HookError> {
        self.rec.push(format!("{}:shutdown:{:?}", self.name, r));
        Ok(())
    }
}

fn ctx() -> HookCtx {
    // The struct fields (agent_name, run_id, etc.) aren't asserted on in
    // this test — it only checks fire sequence — so the helper is fine.
    HookCtx::for_test_with_home(std::path::PathBuf::new(), 0)
}

#[tokio::test]
async fn hook_fire_sequence_telegram_inbound_to_outbound() {
    let rec = Arc::new(Recording::default());
    let chain = HookChain::new(vec![
        Arc::new(RecordHook {
            name: "Telemetry".into(),
            rec: rec.clone(),
        }),
        Arc::new(RecordHook {
            name: "Voice".into(),
            rec: rec.clone(),
        }),
        Arc::new(RecordHook {
            name: "B0".into(),
            rec: rec.clone(),
        }),
        Arc::new(RecordHook {
            name: "Ledger".into(),
            rec: rec.clone(),
        }),
    ]);
    let tok = CancellationToken::new();
    let c = ctx();

    // Inbound from Telegram bridge:
    let env = A2AEnvelopeView {
        method: "message/send".into(),
        from_pubkey: Some("z6Mk-bridge".into()),
        task_id: None,
        raw: serde_json::json!({}),
    };
    chain.on_message_received(&c, &env, &tok).await;
    chain
        .on_trigger_fired(
            &c,
            TriggerKind::Companion,
            &TriggerPayload {
                source: "companion".into(),
                data: serde_json::json!({}),
                received_at: std::time::SystemTime::UNIX_EPOCH,
            },
            &tok,
        )
        .await;
    chain
        .on_prompt_submit(
            &c,
            &PromptView {
                system: None,
                messages: vec![],
            },
            &tok,
        )
        .await;
    let call = ToolCall {
        tool_name: "telegram.send".into(),
        mcp_server: Some("telegram-bridge".into()),
        call_id: "c1".into(),
        input: serde_json::json!({}),
    };
    let _ = chain.pre_tool_use(&c, &call, &tok).await.unwrap();
    chain
        .post_tool_use(
            &c,
            &call,
            &ToolResult {
                call_id: "c1".into(),
                ok: true,
                output: serde_json::json!({}),
                duration_ms: 5,
            },
            &tok,
        )
        .await;
    chain
        .on_step_finish(
            &c,
            &Step {
                model: "claude".into(),
                usage_input_tokens: 10,
                usage_output_tokens: 20,
                finish_reason: "stop".into(),
                was_compaction: false,
            },
            &tok,
        )
        .await;
    chain
        .on_message_send(
            &c,
            &OutboundView {
                recipient: Some("user".into()),
                body: "OK".into(),
                locale: Some("zh-TW".into()),
            },
            &tok,
        )
        .await;

    let events = rec.snapshot();
    insta::assert_yaml_snapshot!(events);
}
