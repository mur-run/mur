//! A2A method: `effort/set` — change the running agent's reasoning effort for
//! this session, without a restart.
//!
//! Effort is a per-call parameter (`task_runner.rs`), so unlike `model/set`
//! this needs no client reconstruction: store the level and the next turn
//! carries it.
//!
//! Two things this handler deliberately does NOT do.
//!
//! It does not narrow the level to what the model accepts. Narrowing already
//! happens at the wire in each client — `mur_common::llm::supported_effort`'s
//! own doc records the split ("requests state the effort they want and this
//! narrows it"). A second narrowing here would be a second derivation of one
//! rule, which is the duplication this whole design exists to prevent.
//!
//! It does not write `profile.yaml`. The persistent form is
//! `mur agent effort`, and murmur's `/effort --save` calls that. A session
//! override that silently persisted would make the two surfaces mean the same
//! thing, and then neither would mean anything.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::protocol::a2a_server::{HandlerError, MethodHandler, RequestContext};
use crate::task_runner::TaskRunner;

pub struct EffortSetHandler {
    runner: Arc<TaskRunner>,
}

impl EffortSetHandler {
    pub fn new(runner: Arc<TaskRunner>) -> Self {
        Self { runner }
    }
}

/// Pull the level out of the params, or `None` to clear back to the profile
/// value. Split out from [`MethodHandler::handle`] so the argument handling is
/// testable without standing up a runner.
fn parse_level(params: Option<Value>) -> Result<Option<mur_common::llm::Effort>, HandlerError> {
    let params = params.ok_or_else(|| HandlerError::InvalidParams("missing params".into()))?;
    match params.get("level") {
        // Explicit null clears the override.
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(raw)) => raw
            .parse::<mur_common::llm::Effort>()
            .map(Some)
            .map_err(HandlerError::InvalidParams),
        Some(other) => Err(HandlerError::InvalidParams(format!(
            "'level' must be a string, got {other}"
        ))),
    }
}

#[async_trait]
impl MethodHandler for EffortSetHandler {
    async fn handle(
        &self,
        params: Option<Value>,
        _ctx: &RequestContext,
    ) -> Result<Value, HandlerError> {
        let level = parse_level(params)?;
        self.runner.set_effort(level);
        tracing::info!(
            level = level.map(|e| e.as_str()).unwrap_or("<cleared>"),
            "effort/set: session effort changed"
        );
        Ok(json!({
            "level": level.map(|e| e.as_str()),
            "effective": "next-turn",
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::llm::Effort;

    #[test]
    fn a_level_string_parses() {
        let p = Some(json!({"level": "xhigh"}));
        assert_eq!(parse_level(p).unwrap(), Some(Effort::Xhigh));
    }

    #[test]
    fn an_explicit_null_clears_the_override() {
        assert_eq!(parse_level(Some(json!({"level": null}))).unwrap(), None);
        assert_eq!(parse_level(Some(json!({}))).unwrap(), None);
    }

    #[test]
    fn a_typo_is_reported_with_the_valid_set_rather_than_ignored() {
        let err = parse_level(Some(json!({"level": "hgih"}))).unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("valid:"), "{msg}");
    }

    #[test]
    fn a_non_string_level_is_rejected_not_coerced() {
        assert!(parse_level(Some(json!({"level": 3}))).is_err());
    }

    #[test]
    fn missing_params_entirely_is_an_error() {
        assert!(parse_level(None).is_err());
    }
}
