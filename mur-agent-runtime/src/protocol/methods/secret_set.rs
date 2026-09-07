//! A2A methods: `secret/set` and `secret/delete` — hand the RUNNING agent a
//! credential, or take one back.
//!
//! The durable half (keychain + `profile.secrets`) is written by the CLI
//! process before it dials this; this is how the sealed runtime learns about
//! it without a restart. The runtime resolves secrets before the sandbox
//! seals and caches them, so writing the keychain alone reaches a running
//! agent only after it restarts — that is the whole reason this method
//! exists. Modelled on `memory/reload`: state changes on disk, then the
//! running agent is told.
//!
//! Nothing here logs, echoes, or returns the value. The response carries the
//! name and the resulting name list only — the CLI prints from that.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::protocol::a2a_server::{HandlerError, MethodHandler, RequestContext};
use crate::secrets::SecretVault;

pub struct SecretSetHandler {
    vault: Arc<SecretVault>,
}

pub struct SecretDeleteHandler {
    vault: Arc<SecretVault>,
}

impl SecretSetHandler {
    pub fn new(vault: Arc<SecretVault>) -> Self {
        Self { vault }
    }
}

impl SecretDeleteHandler {
    pub fn new(vault: Arc<SecretVault>) -> Self {
        Self { vault }
    }
}

fn str_param<'a>(params: &'a Option<Value>, key: &str) -> Result<&'a str, HandlerError> {
    params
        .as_ref()
        .and_then(|p| p.get(key))
        .and_then(Value::as_str)
        .ok_or_else(|| HandlerError::InvalidParams(format!("missing string param '{key}'")))
}

#[async_trait]
impl MethodHandler for SecretSetHandler {
    async fn handle(
        &self,
        params: Option<Value>,
        _ctx: &RequestContext,
    ) -> Result<Value, HandlerError> {
        let name = str_param(&params, "name")?;
        let value = str_param(&params, "value")?;
        self.vault
            .set(name, value)
            .map_err(|e| HandlerError::InvalidParams(e.to_string()))?;
        // Name only. The value is never a field of any log line here.
        tracing::info!(name, "secret/set: vault updated");
        Ok(json!({
            "name": name,
            "names": self.vault.names(),
            "effective": "next-turn",
        }))
    }
}

#[async_trait]
impl MethodHandler for SecretDeleteHandler {
    async fn handle(
        &self,
        params: Option<Value>,
        _ctx: &RequestContext,
    ) -> Result<Value, HandlerError> {
        let name = str_param(&params, "name")?;
        let removed = self.vault.remove(name);
        tracing::info!(name, removed, "secret/delete");
        Ok(json!({
            "name": name,
            "removed": removed,
            "names": self.vault.names(),
            "effective": "next-turn",
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> RequestContext {
        RequestContext::default()
    }

    #[tokio::test]
    async fn set_then_delete_round_trips_and_never_returns_the_value() {
        let vault = Arc::new(SecretVault::new());
        let set = SecretSetHandler::new(vault.clone());
        let res = set
            .handle(
                Some(json!({
                    "name": "GITEA_TOKEN",
                    "value": "d8b04a3cc632a5c8026cf5a810d36e292c603f99"
                })),
                &ctx(),
            )
            .await
            .unwrap();
        assert_eq!(res["name"], "GITEA_TOKEN");
        assert_eq!(res["names"], json!(["GITEA_TOKEN"]));
        assert!(!res.to_string().contains("d8b04a3c"), "{res}");

        let del = SecretDeleteHandler::new(vault.clone());
        let res = del
            .handle(Some(json!({"name": "GITEA_TOKEN"})), &ctx())
            .await
            .unwrap();
        assert_eq!(res["removed"], true);
        assert_eq!(res["names"], json!([]));
        // Deleting what is not there is not an error — the CLI's durable half
        // may have succeeded on an earlier attempt.
        let res = del
            .handle(Some(json!({"name": "GITEA_TOKEN"})), &ctx())
            .await
            .unwrap();
        assert_eq!(res["removed"], false);
    }

    #[tokio::test]
    async fn a_short_value_is_rejected_as_invalid_params() {
        let set = SecretSetHandler::new(Arc::new(SecretVault::new()));
        let err = set
            .handle(Some(json!({"name": "K", "value": "short"})), &ctx())
            .await
            .unwrap_err();
        assert!(matches!(err, HandlerError::InvalidParams(_)), "{err:?}");
    }

    #[tokio::test]
    async fn missing_params_are_invalid_params_not_a_panic() {
        let set = SecretSetHandler::new(Arc::new(SecretVault::new()));
        assert!(matches!(
            set.handle(None, &ctx()).await.unwrap_err(),
            HandlerError::InvalidParams(_)
        ));
        assert!(matches!(
            set.handle(Some(json!({"name": "K"})), &ctx())
                .await
                .unwrap_err(),
            HandlerError::InvalidParams(_)
        ));
    }
}
