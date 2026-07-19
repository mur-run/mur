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
        _call: &ToolCall,
        result: &ToolResult,
        _tok: &CancellationToken,
    ) -> Result<PostToolUsePatch, HookError> {
        if !self.cfg.auto.enabled || !self.cfg.auto.agent_runtime {
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
