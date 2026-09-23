//! `CompressHook` — size-gated auto-compression of agent tool outputs (Surface 2).
//!
//! Mirrors `B0SafetyHook::post_tool_use`: returns a `PostToolUsePatch.replace_output`
//! so the supervisor rewrites `ToolResult.output` before it is recorded / shown to
//! the agent. The patch is consumed by `TaskRunner::apply_post_tool_use` in the
//! agentic loop (same path as B0 rule 8), so the end-to-end offload is now effective.

use std::path::PathBuf;

use tokio_util::sync::CancellationToken;

use mur_compress::{CompressConfig, CompressEngine};

use crate::hooks::{Hook, HookCtx, HookError, PostToolUsePatch, ToolCall, ToolResult};

/// Tools whose output must never be offloaded to a hash placeholder — the
/// runtime-side twin of `AUTO_COMPRESS_SKIP` in `mur-mcp-server/src/tools.rs`.
///
/// * `read_file` — the model asked to SEE those bytes. Compressing the result
///   is the "AI can't read large files" bug: every big file collapses to a
///   three-line stub and the model is forced into dozens of 20–60-line
///   `offset`/`limit` windows (or a retrieve round-trip) to read it at all.
///   The tool already caps itself at `MAX_RETURN_BYTES`.
/// * `mur_retrieve` (any MCP prefix) — its whole job is to return the original;
///   re-compressing it is a loop.
/// * `mur_compress` / `mur_compress_stats` / `mur_job_status` — mirror the MCP
///   surface so behaviour is the same whichever door the tool came through.
const AUTO_COMPRESS_SKIP: &[&str] = &[
    "read_file",
    "mur_compress",
    "mur_retrieve",
    "mur_compress_stats",
    "mur_job_status",
];

/// True when `tool_name` (bare or `mcp__<server>__<tool>` wire form) is exempt.
fn is_exempt(tool_name: &str) -> bool {
    let bare = tool_name.rsplit("__").next().unwrap_or(tool_name);
    AUTO_COMPRESS_SKIP.contains(&tool_name) || AUTO_COMPRESS_SKIP.contains(&bare)
}

/// Auto-compresses oversized tool outputs for MUR's own spawned agents.
pub struct CompressHook {
    /// CCR store dir, i.e. `<mur_home>/compress`.
    dir: PathBuf,
    /// Loaded compression config (carries the `auto` gates).
    cfg: CompressConfig,
}

impl CompressHook {
    pub fn new(dir: PathBuf, cfg: CompressConfig) -> Self {
        Self { dir, cfg }
    }
}

#[async_trait::async_trait]
impl Hook for CompressHook {
    async fn post_tool_use(
        &self,
        _ctx: &HookCtx,
        call: &ToolCall,
        result: &ToolResult,
        _tok: &CancellationToken,
    ) -> Result<PostToolUsePatch, HookError> {
        if !self.cfg.auto.enabled || !self.cfg.auto.agent_runtime || is_exempt(&call.tool_name) {
            return Ok(PostToolUsePatch::default());
        }
        // Per-call engine (cheap; mirrors the CLI/MCP per-call pattern and keeps
        // the hook `Send + Sync` without holding a non-Sync tokenizer).
        let engine = match CompressEngine::new(&self.dir, self.cfg.clone()) {
            Ok(e) => e,
            Err(_) => return Ok(PostToolUsePatch::default()),
        };
        // A failed tool result (`ok == false`) must NEVER be offloaded to a
        // hash placeholder — doing so hides the `"tool error: ..."` behind a
        // retrieval hop and lets it be mistaken for success. Pass the error
        // signal to the guarded compressor, which passes such results through
        // unchanged and annotates any residual bulk offload with an error count.
        match mur_compress::auto_compress_value_guarded(
            &engine,
            &result.output,
            None,
            self.cfg.auto.min_tokens,
            !result.ok,
        ) {
            Some(replacement) => Ok(PostToolUsePatch {
                replace_output: Some(replacement),
            }),
            None => Ok(PostToolUsePatch::default()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::{Hook, HookCtx};
    use serde_json::json;

    fn big_output() -> serde_json::Value {
        // Well past the MIN_TOKENS_FLOOR (500 tokens) so the size gate opens.
        let body: String = (0..2_000)
            .map(|i| format!("line {i}: the quick brown fox jumps over the lazy dog\n"))
            .collect();
        json!({ "content": body })
    }

    fn hook() -> (tempfile::TempDir, CompressHook) {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = CompressConfig::default();
        cfg.auto.enabled = true;
        cfg.auto.agent_runtime = true;
        let h = CompressHook::new(dir.path().to_path_buf(), cfg);
        (dir, h)
    }

    fn call(name: &str) -> ToolCall {
        ToolCall {
            tool_name: name.into(),
            mcp_server: None,
            call_id: "c1".into(),
            input: json!({}),
        }
    }

    fn result() -> ToolResult {
        ToolResult {
            call_id: "c1".into(),
            ok: true,
            output: big_output(),
            duration_ms: 1,
        }
    }

    #[tokio::test]
    async fn read_file_output_is_never_offloaded() {
        let (_d, h) = hook();
        let ctx = HookCtx::for_test_with_home(PathBuf::new(), 0);
        let patch = h
            .post_tool_use(
                &ctx,
                &call("read_file"),
                &result(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            patch.replace_output.is_none(),
            "read_file is an explicit request to SEE the bytes; compressing it forces the model into 20-line windows"
        );
    }

    #[tokio::test]
    async fn bash_output_is_still_offloaded() {
        let (_d, h) = hook();
        let ctx = HookCtx::for_test_with_home(PathBuf::new(), 0);
        let patch = h
            .post_tool_use(&ctx, &call("bash"), &result(), &CancellationToken::new())
            .await
            .unwrap();
        assert!(
            patch.replace_output.is_some(),
            "size gate must still fire for bash"
        );
    }

    #[tokio::test]
    async fn retrieve_output_is_never_offloaded() {
        let (_d, h) = hook();
        let ctx = HookCtx::for_test_with_home(PathBuf::new(), 0);
        let patch = h
            .post_tool_use(
                &ctx,
                &call("mcp__media__mur_retrieve"),
                &result(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            patch.replace_output.is_none(),
            "re-compressing a retrieval is a loop"
        );
    }
}
