//! #2488: owner methods are served to stdio, or to a caller with a proof
//! keyed by the home token and bound to this agent — never to a peer that
//! merely passed `accepts_from`, and not to the agent's own lineage either.

use super::*;
use crate::hitl::authority::ApprovalAuthority;
use mur_common::owner_call::{PARAM, proof};

const TOKEN: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
const AGENT: &str = "alice";

/// Echoes its params, so a test can see what the handler received.
struct Echo;

#[async_trait]
impl MethodHandler for Echo {
    async fn handle(&self, p: Option<Value>, _c: &RequestContext) -> Result<Value, HandlerError> {
        Ok(p.unwrap_or(Value::Null))
    }
}

fn dispatcher(authority: ApprovalAuthority) -> Dispatcher {
    let mut d = Dispatcher::new().with_owner_gate(AGENT, authority);
    for m in ["secret/set", "tasks/cancel", "message/send"] {
        d.register(m, Box::new(Echo));
    }
    d
}

fn sealed() -> ApprovalAuthority {
    ApprovalAuthority::new(Some(TOKEN.into()), true)
}

async fn call(
    d: &Dispatcher,
    method: &str,
    params: Value,
    ctx: &RequestContext,
) -> JsonRpcResponse {
    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: method.into(),
        params: Some(params),
    };
    d.dispatch(req, ctx).await.unwrap()
}

fn denied(r: &JsonRpcResponse) -> bool {
    r.result.is_none() && r.error.as_ref().is_some_and(|e| e.code == -32011)
}

#[tokio::test]
async fn a_peer_without_proof_cannot_operate_the_agent() {
    let d = dispatcher(sealed());
    // A peer socket connection, and the agent's own lineage: neither is the owner.
    let lineage = RequestContext::none().with_runs_tools(true);
    for ctx in [RequestContext::none(), lineage] {
        for m in ["secret/set", "tasks/cancel"] {
            let r = call(&d, m, json!({"name": "K", "value": "v"}), &ctx).await;
            assert!(denied(&r), "{m}: {r:?}");
        }
    }
}

#[tokio::test]
async fn conversation_methods_stay_open_to_peers() {
    let d = dispatcher(sealed());
    let r = call(&d, "message/send", json!({"x": 1}), &RequestContext::none()).await;
    assert_eq!(r.result, Some(json!({"x": 1})));
}

#[tokio::test]
async fn a_valid_proof_is_admitted_and_stripped_before_the_handler() {
    let d = dispatcher(sealed());
    let p = json!({"name": "K", PARAM: proof(TOKEN, AGENT, "secret/set")});
    let r = call(&d, "secret/set", p, &RequestContext::none()).await;
    assert_eq!(r.result, Some(json!({"name": "K"})), "{r:?}");
}

#[tokio::test]
async fn a_proof_for_another_agent_method_or_home_is_refused() {
    let d = dispatcher(sealed());
    for bad in [
        proof(TOKEN, "bob", "secret/set"),
        proof(TOKEN, AGENT, "tasks/cancel"),
        proof("ff", AGENT, "secret/set"),
        TOKEN.to_string(),
        String::new(),
    ] {
        let r = call(
            &d,
            "secret/set",
            json!({ PARAM: bad }),
            &RequestContext::none(),
        )
        .await;
        assert!(denied(&r), "{bad:?}: {r:?}");
    }
}

#[tokio::test]
async fn stdio_is_the_owner_without_proof() {
    let d = dispatcher(sealed());
    let r = call(
        &d,
        "tasks/cancel",
        json!({"id": "t"}),
        &RequestContext::owner(),
    )
    .await;
    assert_eq!(r.result, Some(json!({"id": "t"})));
}

#[tokio::test]
async fn no_token_or_no_gate_fails_closed_with_a_reason() {
    let tokenless = dispatcher(ApprovalAuthority::new(None, true));
    let p = json!({ PARAM: proof(TOKEN, AGENT, "secret/set") });
    let r = call(&tokenless, "secret/set", p.clone(), &RequestContext::none()).await;
    assert!(denied(&r));
    assert!(r.error.unwrap().message.contains("restart the agent"));

    let mut ungated = Dispatcher::new();
    ungated.register("secret/set", Box::new(Echo));
    assert!(denied(
        &call(&ungated, "secret/set", p, &RequestContext::none()).await
    ));
}

#[tokio::test]
async fn an_unsealed_boot_still_honours_the_owner() {
    // The seal guards approvals; owner controls must not lock the user out.
    let d = dispatcher(ApprovalAuthority::new(Some(TOKEN.into()), false));
    let p = json!({ PARAM: proof(TOKEN, AGENT, "tasks/cancel") });
    assert!(
        call(&d, "tasks/cancel", p, &RequestContext::none())
            .await
            .result
            .is_some()
    );
}

#[tokio::test]
async fn a_stray_proof_never_reaches_a_peer_method_handler() {
    let d = dispatcher(sealed());
    let p = json!({"x": 1, PARAM: "anything"});
    let r = call(&d, "message/send", p, &RequestContext::none()).await;
    assert_eq!(r.result, Some(json!({"x": 1})));
}
