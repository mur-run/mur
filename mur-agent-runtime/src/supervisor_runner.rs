//! Extracted helper: build a TaskRunner with skills + hook chain wired in.
//! Keeps supervisor.rs under 800 lines per CLAUDE.md ceiling.

use std::sync::Arc;

use crate::hooks::{HookChain, HookCtx};
use crate::llm::LlmClient;
use crate::skills::RuntimeSkills;
use crate::task_runner::TaskRunner;
use mur_common::config::SkillsConfig;
use tokio_util::sync::CancellationToken;

pub fn build_runner(
    client: Arc<dyn LlmClient>,
    base_system_prompt: Option<String>,
    skills: Arc<RuntimeSkills>,
    skills_cfg: SkillsConfig,
    hook_chain: Option<Arc<HookChain>>,
    hook_ctx: Option<HookCtx>,
    hook_cancel: Option<CancellationToken>,
) -> Arc<TaskRunner> {
    let mut runner = TaskRunner::with_llm(client)
        .with_system_prompt(base_system_prompt)
        .with_skills(skills)
        .with_skills_cfg(skills_cfg);
    if let (Some(chain), Some(ctx), Some(cancel)) = (hook_chain, hook_ctx, hook_cancel) {
        runner = runner.with_hook_chain(chain, ctx, cancel);
    }
    Arc::new(runner)
}
