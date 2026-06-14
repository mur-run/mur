//! `CompressHook` — size-gated auto-compression of agent tool outputs (Surface 2).
//!
//! Mirrors `B0SafetyHook::post_tool_use`: returns a `PostToolUsePatch.replace_output`
//! so the supervisor can rewrite `ToolResult.output` before it is recorded / shown
//! to the agent. Like B0 rule 8, the end-to-end effect depends on the supervisor
//! consuming `replace_output` (tracked separately); the hook + chain folding are
//! complete and tested here.

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
        match mur_compress::auto_compress_value(
            &engine,
            &result.output,
            None,
            self.cfg.auto.min_tokens,
        ) {
            Some(replacement) => Ok(PostToolUsePatch {
                replace_output: Some(replacement),
            }),
            None => Ok(PostToolUsePatch::default()),
        }
    }
}
