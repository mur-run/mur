//! A2A method `egress/grant` — let a host through a RUNNING agent's egress
//! proxy without a restart (#1907).
//!
//! A grant widens what a sandboxed MCP server may reach, and the agent socket
//! is reachable by the agent and everything it spawns. So a grant is held to
//! the same bar as a HITL `allow`: an enforcing seal plus this MUR home's
//! approval token ([`ApprovalAuthority`]). Without that, the agent could ask
//! its own proxy for any host.
//!
//! `session: true` is in-memory only and gone at restart. `session: false` is
//! the live half of a permanent grant the CLI has already written to the
//! profile; the runtime treats both the same, the profile is what persists.

use async_trait::async_trait;
use mur_common::egress_event::EgressGrant;
use serde_json::{Value, json};

use crate::hitl::authority::ApprovalAuthority;
use crate::protocol::a2a_server::{HandlerError, MethodHandler, RequestContext};
use crate::sandbox::egress_proxy::EgressProxyHandle;

pub struct EgressGrantHandler {
    proxy: EgressProxyHandle,
    authority: ApprovalAuthority,
}

impl EgressGrantHandler {
    pub fn new(proxy: EgressProxyHandle, authority: ApprovalAuthority) -> Self {
        Self { proxy, authority }
    }
}

#[async_trait]
impl MethodHandler for EgressGrantHandler {
    async fn handle(
        &self,
        params: Option<Value>,
        _ctx: &RequestContext,
    ) -> Result<Value, HandlerError> {
        let p = params.ok_or_else(|| HandlerError::InvalidParams("missing params".into()))?;
        let token = p[mur_common::hitl::approval_token::PARAM]
            .as_str()
            .map(str::to_owned);
        let g: EgressGrant = serde_json::from_value(p)
            .map_err(|e| HandlerError::InvalidParams(format!("egress/grant: {e}")))?;
        self.authority.check(true, token.as_deref()).map_err(|r| {
            tracing::warn!(server = %g.server, host = %g.host, refusal = ?r, "refused an unauthenticated egress grant");
            HandlerError::ApprovalRefused(r.message().to_string())
        })?;
        let host = self
            .proxy
            .grant(&g.server, &g.host)
            .map_err(|e| HandlerError::InvalidParams(e.to_string()))?;
        tracing::info!(server = %g.server, host = %host, session = g.session, "egress/grant applied");
        Ok(json!({ "server": g.server, "host": host, "session": g.session }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handler(authority: ApprovalAuthority) -> (EgressGrantHandler, EgressProxyHandle) {
        let proxy = EgressProxyHandle::for_test("127.0.0.1:1".parse().unwrap());
        proxy.register_server("browser", vec![], vec![], false);
        (EgressGrantHandler::new(proxy.clone(), authority), proxy)
    }

    #[tokio::test]
    async fn grant_without_the_approval_token_is_refused() {
        let (h, proxy) = handler(ApprovalAuthority::new(Some("tok".into()), true));
        let err = h
            .handle(
                Some(json!({ "server": "browser", "host": "a.example.com" })),
                &RequestContext::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, HandlerError::ApprovalRefused(_)), "{err}");
        let _ = proxy;
    }

    #[tokio::test]
    async fn grant_with_token_applies_and_returns_the_normalized_host() {
        let (h, _) = handler(ApprovalAuthority::new(Some("tok".into()), true));
        let v = h
            .handle(
                Some(json!({
                    "server": "browser",
                    "host": "IMG.Example.com",
                    "session": true,
                    mur_common::hitl::approval_token::PARAM: "tok",
                })),
                &RequestContext::default(),
            )
            .await
            .unwrap();
        assert_eq!(v["host"], "img.example.com");
        assert_eq!(v["session"], true);
    }

    #[tokio::test]
    async fn advisory_seal_refuses_even_with_token() {
        let (h, _) = handler(ApprovalAuthority::new(Some("tok".into()), false));
        let err = h
            .handle(
                Some(json!({
                    "server": "browser",
                    "host": "a.example.com",
                    mur_common::hitl::approval_token::PARAM: "tok",
                })),
                &RequestContext::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, HandlerError::ApprovalRefused(_)));
    }
}
