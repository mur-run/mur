//! A2A method: `skills/get` — serve a skill manifest by name.
//!
//! In M4a this handler is registered with the dispatcher but install does
//! not yet reach it over a socket — install reads the local store directly.
//! M4b wires the real round-trip.

use async_trait::async_trait;
use mur_common::skill::{
    content_hash_for_trust, global_skill_dir, read_from_dir, serialize_canonical,
};
use serde_json::{Value, json};
use std::path::PathBuf;

use crate::protocol::a2a_server::{HandlerError, MethodHandler, RequestContext};

pub struct SkillsGetHandler {
    /// Root of the skill store this handler serves from. In M4a this is
    /// `MUR_HOME`; skills resolve to `<root>/skills/<name>/skill.yaml`.
    store_root: PathBuf,
}

impl SkillsGetHandler {
    pub fn new(store_root: PathBuf) -> Self {
        Self { store_root }
    }
}

#[async_trait]
impl MethodHandler for SkillsGetHandler {
    async fn handle(
        &self,
        params: Option<Value>,
        _ctx: &RequestContext,
    ) -> Result<Value, HandlerError> {
        let params = params.ok_or_else(|| HandlerError::InvalidParams("missing params".into()))?;

        let skill_name = params
            .get("skill")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HandlerError::InvalidParams("missing 'skill' field".into()))?;

        // `skill` comes from a remote A2A peer and is joined into
        // `<store_root>/skills/<name>`. Reject path-traversal so a peer can't
        // read another agent's skill (or any `skill.yaml`/`skill.md`) outside
        // the store via `../…`.
        if !mur_common::skill::loader::is_valid_skill_name(skill_name) {
            return Err(HandlerError::InvalidParams(format!(
                "invalid skill name: {skill_name:?}"
            )));
        }

        let dir = global_skill_dir(&self.store_root, skill_name);
        let manifest = read_from_dir(&dir)
            .map_err(|e| HandlerError::Internal(format!("skill '{skill_name}' not found: {e}")))?;

        let hash = content_hash_for_trust(&manifest)
            .map_err(|e| HandlerError::Internal(format!("hash failed: {e}")))?;

        let yaml = serialize_canonical(&manifest)
            .map_err(|e| HandlerError::Internal(format!("serialize failed: {e}")))?;

        Ok(json!({
            "manifest": yaml,
            "content_sha256": hash,
            "transfer_chain": manifest.transfer_chain,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::skill::{parse_canonical, write_to_dir};
    use tempfile::tempdir;

    #[tokio::test]
    async fn serves_installed_skill() {
        let dir = tempdir().unwrap();
        let m = parse_canonical(
            r#"
name: test-skill
version: 1.0.0
publisher: human:t
description: d
category: context
content:
  abstract: a
  context: b
"#,
        )
        .unwrap();
        write_to_dir(&global_skill_dir(dir.path(), "test-skill"), &m).unwrap();

        let handler = SkillsGetHandler::new(dir.path().to_path_buf());
        let result = handler
            .handle(
                Some(json!({"skill": "test-skill"})),
                &RequestContext::none(),
            )
            .await
            .unwrap();

        assert!(result["manifest"].as_str().unwrap().contains("test-skill"));
        assert_eq!(result["content_sha256"].as_str().unwrap().len(), 64);
        assert!(result["transfer_chain"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn missing_skill_returns_internal_error() {
        let dir = tempdir().unwrap();
        let handler = SkillsGetHandler::new(dir.path().to_path_buf());
        let err = handler
            .handle(Some(json!({"skill": "nope"})), &RequestContext::none())
            .await
            .unwrap_err();
        assert!(matches!(err, HandlerError::Internal(_)));
    }

    #[tokio::test]
    async fn missing_skill_param_returns_invalid_params() {
        let dir = tempdir().unwrap();
        let handler = SkillsGetHandler::new(dir.path().to_path_buf());
        let err = handler
            .handle(Some(json!({})), &RequestContext::none())
            .await
            .unwrap_err();
        assert!(matches!(err, HandlerError::InvalidParams(_)));
    }

    #[tokio::test]
    async fn rejects_path_traversal_skill_name() {
        // A remote peer must not read a skill outside the store via `../…`. The
        // dangerous (cross-agent / arbitrary-dir) forms all contain a separator,
        // which is_valid_skill_name refuses.
        let dir = tempdir().unwrap();
        let handler = SkillsGetHandler::new(dir.path().to_path_buf());
        for bad in [
            "../secret",
            "a/b",
            "../../etc/passwd",
            "../agents/victim/skills/x",
            "a\\b",
        ] {
            let err = handler
                .handle(Some(json!({ "skill": bad })), &RequestContext::none())
                .await
                .unwrap_err();
            assert!(
                matches!(err, HandlerError::InvalidParams(_)),
                "{bad:?} should be rejected, got {err:?}"
            );
        }
    }
}
