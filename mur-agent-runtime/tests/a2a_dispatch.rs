use async_trait::async_trait;
use mur_agent_runtime::protocol::a2a_server::{
    Dispatcher, HandlerError, MethodHandler, RequestContext,
};
use mur_common::JsonRpcRequest;
use serde_json::{Value, json};

struct Echo;

#[async_trait]
impl MethodHandler for Echo {
    async fn handle(
        &self,
        params: Option<Value>,
        _ctx: &RequestContext,
    ) -> Result<Value, HandlerError> {
        Ok(params.unwrap_or(json!(null)))
    }
}

#[tokio::test]
async fn dispatches_known_method() {
    let mut d = Dispatcher::new();
    d.register("echo", Box::new(Echo));
    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(1)),
        method: "echo".into(),
        params: Some(json!("hi")),
    };
    let resp = d.dispatch(req, &RequestContext::none()).await.unwrap();
    assert_eq!(resp.result, Some(json!("hi")));
    assert!(resp.error.is_none());
}

#[tokio::test]
async fn returns_method_not_found_with_code_neg32601() {
    let d = Dispatcher::new();
    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(2)),
        method: "nope".into(),
        params: None,
    };
    let resp = d.dispatch(req, &RequestContext::none()).await.unwrap();
    assert_eq!(resp.error.as_ref().unwrap().code, -32601);
}

#[tokio::test]
async fn maps_handler_error_to_custom_code() {
    struct Fails;
    #[async_trait]
    impl MethodHandler for Fails {
        async fn handle(
            &self,
            _p: Option<Value>,
            _ctx: &RequestContext,
        ) -> Result<Value, HandlerError> {
            Err(HandlerError::CommunicationDenied("caller_x".into()))
        }
    }
    let mut d = Dispatcher::new();
    d.register("x", Box::new(Fails));
    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(3)),
        method: "x".into(),
        params: None,
    };
    let resp = d.dispatch(req, &RequestContext::none()).await.unwrap();
    assert_eq!(resp.error.as_ref().unwrap().code, -32011);
}
