//! Extracted helper: build a TaskRunner with skills wired in.
//! Keeps supervisor.rs under 800 lines per CLAUDE.md ceiling.

use std::sync::Arc;

use crate::llm::LlmClient;
use crate::skills::RuntimeSkills;
use crate::task_runner::TaskRunner;
use mur_common::config::SkillsConfig;

pub fn build_runner(
    client: Arc<dyn LlmClient>,
    base_system_prompt: Option<String>,
    skills: Arc<RuntimeSkills>,
    skills_cfg: SkillsConfig,
) -> Arc<TaskRunner> {
    Arc::new(
        TaskRunner::with_llm(client)
            .with_system_prompt(base_system_prompt)
            .with_skills(skills)
            .with_skills_cfg(skills_cfg),
    )
}
