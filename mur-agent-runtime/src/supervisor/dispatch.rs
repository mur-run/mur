//! A2A method dispatcher wiring and the HITL response handlers.

use super::*;

pub(super) struct HitlRespondHandler {
    pub(super) pending_approvals:
        Arc<Mutex<HashMap<String, oneshot::Sender<crate::hitl::HitlDecision>>>>,
    pub(super) authority: crate::hitl::authority::ApprovalAuthority,
    pub(super) shim_trust: crate::hitl::shim_ticket::ShimTrust,
}

#[async_trait::async_trait]
impl crate::protocol::a2a_server::MethodHandler for HitlRespondHandler {
    async fn handle(
        &self,
        params: Option<serde_json::Value>,
        ctx: &crate::protocol::a2a_server::RequestContext,
    ) -> Result<serde_json::Value, crate::protocol::a2a_server::HandlerError> {
        let p = params.ok_or_else(|| {
            crate::protocol::a2a_server::HandlerError::InvalidParams("missing params".into())
        })?;
        let hitl_id = p["hitl_id"]
            .as_str()
            .ok_or_else(|| {
                crate::protocol::a2a_server::HandlerError::InvalidParams("missing hitl_id".into())
            })?
            .to_string();
        let allow = p["allow"].as_bool().ok_or_else(|| {
            crate::protocol::a2a_server::HandlerError::InvalidParams("missing allow".into())
        })?;
        let reason = p["reason"].as_str().map(str::to_string);
        let surface = p["surface"].as_str().map(str::to_string);
        // Checked BEFORE the pending entry is taken: a refused allow must not
        // consume the gate, or a spawned tool could burn the human's answer.
        let trusted_shim = self.shim_trust.authorizes(ctx.conn.as_deref(), &hitl_id);
        self.authority
            .check_from(
                allow,
                p[mur_common::hitl::approval_token::PARAM].as_str(),
                trusted_shim,
            )
            .map_err(|r| {
                tracing::warn!(hitl_id = %hitl_id, refusal = ?r, "refused an unauthenticated HITL allow");
                crate::protocol::a2a_server::HandlerError::ApprovalRefused(r.message().to_string())
            })?;
        let tx = self
            .pending_approvals
            .lock()
            .await
            .remove(&hitl_id)
            .ok_or_else(|| {
                // Not-found here means the approval window already closed: the
                // gate timed out (and auto-denied) before the decision arrived,
                // or a decision was already delivered. A generic TaskNotFound
                // read as a cryptic JSON-RPC blob in murmur; say what actually
                // happened and what to do about it.
                crate::protocol::a2a_server::HandlerError::ApprovalExpired(
                    "this approval window already closed — the tool call was auto-denied \
                     at timeout (or the decision was already delivered); re-run the request"
                        .to_string(),
                )
            })?;
        let _ = tx.send(crate::hitl::HitlDecision {
            allow,
            reason,
            surface,
        });
        Ok(serde_json::json!({}))
    }
}

pub(super) struct HitlTestRequestHandler {
    pub(super) pending_approvals:
        Arc<Mutex<HashMap<String, oneshot::Sender<crate::hitl::HitlDecision>>>>,
    pub(super) notifier: tokio::sync::mpsc::Sender<serde_json::Value>,
}

#[async_trait::async_trait]
impl crate::protocol::a2a_server::MethodHandler for HitlTestRequestHandler {
    async fn handle(
        &self,
        params: Option<serde_json::Value>,
        ctx: &crate::protocol::a2a_server::RequestContext,
    ) -> Result<serde_json::Value, crate::protocol::a2a_server::HandlerError> {
        let p = params.unwrap_or(serde_json::json!({}));
        let tool_name = p["tool_name"].as_str().unwrap_or("bash").to_string();
        let tool_input = p["tool_input"].clone();
        let timeout_secs = p["timeout_secs"].as_u64().unwrap_or(300);
        let hitl_id = uuid::Uuid::now_v7().to_string();
        let (tx, _rx) = oneshot::channel::<crate::hitl::HitlDecision>();
        self.pending_approvals
            .lock()
            .await
            .insert(hitl_id.clone(), tx);
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tool/approval_needed",
            "params": {
                "hitl_id": hitl_id,
                "tool_name": tool_name,
                "tool_input": tool_input,
                "prompt": format!("Run `{tool_name}`?"),
                "timeout_ms": timeout_secs * 1000,
            }
        });
        // Route to the issuing connection when present (diagnostic method, but
        // it must not broadcast to other clients either); fall back otherwise.
        let _ = ctx
            .notifier
            .as_ref()
            .unwrap_or(&self.notifier)
            .send(notification)
            .await;
        Ok(serde_json::json!({"hitl_id": hitl_id}))
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_dispatcher(
    profile: &Arc<Profile>,
    runner: &Arc<TaskRunner>,
    mur_home: &Path,
    notifier: tokio::sync::mpsc::Sender<serde_json::Value>,
    pending_approvals: Arc<Mutex<HashMap<String, oneshot::Sender<crate::hitl::HitlDecision>>>>,
    identity: &Arc<AgentIdentity>,
    agent_name: &str,
    key_version: u32,
    model_switch: Option<Arc<crate::llm::switchable::ModelSwitchHandle>>,
    runtime_skills: Arc<crate::skills::RuntimeSkills>,
    secrets: Arc<crate::secrets::SecretVault>,
    approval_authority: crate::hitl::authority::ApprovalAuthority,
) -> Dispatcher {
    let mut d = Dispatcher::new();
    d.register("agent/card", Box::new(CardHandler::new(profile.clone())));
    d.register(
        "message/send",
        Box::new(MessageSendHandler::new(runner.clone()).with_streaming(notifier.clone())),
    );
    d.register(
        "channel/delegate",
        Box::new(
            crate::protocol::methods::channel_delegate::ChannelDelegateHandler::new(
                runner.clone(),
                identity.clone(),
                agent_name.to_string(),
                key_version,
                mur_home.to_path_buf(),
            ),
        ),
    );
    d.register("tasks/get", Box::new(TasksGetHandler::new(runner.clone())));
    d.register(
        "tasks/cancel",
        Box::new(TasksCancelHandler::new(runner.clone())),
    );
    d.register(
        "tasks/list",
        Box::new(TasksListHandler::new(runner.clone())),
    );
    d.register(
        "tools/list",
        Box::new(crate::protocol::methods::tools::ToolsListHandler::new(
            runner.clone(),
        )),
    );
    d.register(
        "tools/call",
        Box::new(crate::protocol::methods::tools::ToolsCallHandler::new(
            runner.clone(),
        )),
    );
    d.register(
        "turn/steer",
        Box::new(crate::protocol::methods::turn::TurnSteerHandler {
            runner: runner.clone(),
        }),
    );
    d.register(
        "skills/get",
        Box::new(crate::protocol::methods::skills::SkillsGetHandler::new(
            mur_home.to_path_buf(),
        )),
    );
    d.register(
        "tool/hitl_respond",
        Box::new(HitlRespondHandler {
            pending_approvals: pending_approvals.clone(),
            authority: approval_authority,
            shim_trust: runner.shim_trust(),
        }),
    );
    d.register(
        "shim/hello",
        Box::new(crate::protocol::methods::shim::ShimHelloHandler {
            trust: runner.shim_trust(),
        }),
    );
    d.register(
        "tool/test_hitl_request",
        Box::new(HitlTestRequestHandler {
            pending_approvals,
            notifier,
        }),
    );
    // murmur /model hot-switch. Single-model agents swap the client; chain
    // and routing agents swap the primary and keep the chain
    // (`chain_switch_handle`). Only echo/misconfigured agents get no handle —
    // they surface method-not-found and the TUI degrades to a profile write +
    // restart hint.
    if let Some(switch) = model_switch {
        d.register(
            "model/set",
            Box::new(crate::protocol::methods::model_set::ModelSetHandler::new(
                switch,
            )),
        );
    }
    // murmur /effort. Unlike model/set this is registered unconditionally:
    // effort is a per-call parameter, so there is no client to rebuild and no
    // agent shape that cannot accept one.
    d.register(
        "effort/set",
        Box::new(crate::protocol::methods::effort_set::EffortSetHandler::new(
            runner.clone(),
        )),
    );
    // murmur `/remember` and `/forget` run in the CLI process; this is how they
    // tell the running agent its memory set changed.
    d.register(
        "memory/reload",
        Box::new(crate::protocol::methods::memory_reload::MemoryReloadHandler::new(runtime_skills)),
    );
    // murmur `/secret`. The CLI has already written the keychain and
    // `profile.secrets`; this is how the sealed runtime learns without a
    // restart. Registered unconditionally: every agent shape can carry an
    // environment variable.
    d.register(
        "secret/set",
        Box::new(crate::protocol::methods::secret_set::SecretSetHandler::new(
            secrets.clone(),
        )),
    );
    d.register(
        "secret/delete",
        Box::new(crate::protocol::methods::secret_set::SecretDeleteHandler::new(secrets)),
    );
    d
}
