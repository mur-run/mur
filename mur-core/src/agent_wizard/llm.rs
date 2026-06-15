//! LLM stages: author skills and draft the DoD prompt from a role, via an LlmClient.
use crate::agent_wizard::draft::{RoleSpec, SkillDraft};
use crate::agent_wizard::research::ResearchNote;
use mur_common::error::LlmError;
use std::future::Future;
use std::pin::Pin;

/// Object-safe wrapper around `LlmClient`. `LlmClient` uses RPITIT and is therefore
/// not dyn-compatible (E0038). This thin trait boxes the future so `dyn WizardLlm` works.
pub trait WizardLlm: Send + Sync {
    fn complete<'a>(
        &'a self,
        prompt: &'a str,
        system: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<String, LlmError>> + Send + 'a>>;
}

/// Blanket impl: any `LlmClient` is also a `WizardLlm`.
impl<L: mur_common::llm::LlmClient + Send + Sync> WizardLlm for L {
    fn complete<'a>(
        &'a self,
        p: &'a str,
        s: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<String, LlmError>> + Send + 'a>> {
        Box::pin(mur_common::llm::LlmClient::complete(self, p, s))
    }
}

/// Build the per-skill authoring prompt for one topic.
fn skill_prompt(role: &RoleSpec, topic: &str, notes: &[ResearchNote]) -> String {
    let mut p = format!(
        "Write a MUR agent skill manifest as a single YAML document for the skill topic \
\"{topic}\", for the agent role \"{dn}\" ({charter}).\n\
Output ONLY the YAML (no markdown fences). It MUST have these top-level keys: name (== \"{topic}\"), \
version: 1.0.0, publisher: human:{name}, a third-person trigger-rich `description`, \
category: context, hosts: [mur-agent], priority: normal, tags, triggers (session_start + a \
keyword regex + a /command), and content with `abstract` and `context`. The `context` body must be \
5-8 imperative rules, each with a one-line *why*.\n",
        topic = topic,
        dn = role.display_name,
        charter = role.charter,
        name = role.name,
    );
    if !notes.is_empty() {
        p.push_str("\nGround the rules in these researched notes (cite where relevant):\n");
        for n in notes {
            p.push_str(&format!("- {} ({})\n", n.summary, n.url));
        }
    }
    p
}

/// Author one skill per topic via the LLM, validating each; one repair retry on invalid YAML.
pub async fn author_skills_llm(
    llm: &dyn WizardLlm,
    role: &RoleSpec,
    topics: &[String],
    notes: &[ResearchNote],
) -> Result<Vec<SkillDraft>, LlmError> {
    let mut out = Vec::new();
    for topic in topics {
        let prompt = skill_prompt(role, topic, notes);
        let yaml = author_one(llm, &prompt).await?;
        out.push(SkillDraft {
            name: topic.clone(),
            yaml,
        });
    }
    Ok(out)
}

/// Call the model, strip stray fences, and validate; on invalid, ask once to fix.
async fn author_one(llm: &dyn WizardLlm, prompt: &str) -> Result<String, LlmError> {
    let sys = "You are an expert author of MUR agent skills. Output only valid YAML.";
    let raw = strip_fences(&llm.complete(prompt, Some(sys)).await?);
    let valid = mur_common::skill::parse_canonical(&raw)
        .map_err(|e| LlmError::Other(e.to_string()))
        .and_then(|m| mur_common::skill::validate(&m).map_err(|e| LlmError::Other(e.to_string())))
        .is_ok();
    if valid {
        return Ok(raw);
    }
    let fix = format!(
        "The YAML you produced was not a valid MUR skill. Fix it and output ONLY corrected YAML.\n\
Previous output:\n{raw}"
    );
    Ok(strip_fences(&llm.complete(&fix, Some(sys)).await?))
}

/// Draft a DoD system prompt for the role via the LLM. The result must carry the
/// honesty rule; if the model omits it, append it (defense-in-depth, not a silent trust).
pub async fn draft_prompt_llm(llm: &dyn WizardLlm, role: &RoleSpec) -> Result<String, LlmError> {
    let sys = "You write system prompts for specialized AI agents. Be concise and concrete.";
    let prompt = format!(
        "Write a system prompt (markdown) for an agent named \"{dn}\" whose charter is: {charter}.\n\
Include: a persona line; an operating-discipline / definition-of-done gate the agent must satisfy \
before claiming a task done; HITL rules (irreversible actions need explicit human confirmation); \
an honesty rule stating it must never fabricate command or file output; and a short 'narrate for a \
watching human' line. Risk level: {risk:?}.",
        dn = role.display_name,
        charter = role.charter,
        risk = role.risk,
    );
    let mut md = llm.complete(&prompt, Some(sys)).await?;
    if !md.to_lowercase().contains("never fabricate") {
        md.push_str(
            "\n\n## Honesty\nYou must never fabricate command or file output. If you can't \
run a tool or read a file this turn, say so and reason from context.\n",
        );
    }
    Ok(md)
}

/// Remove ```yaml / ``` fences a model may wrap output in.
fn strip_fences(s: &str) -> String {
    let t = s.trim();
    let t = t
        .strip_prefix("```yaml")
        .or_else(|| t.strip_prefix("```"))
        .unwrap_or(t);
    t.trim_end_matches("```").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_wizard::draft::RiskLevel;

    /// A canned LlmClient that returns a fixed valid skill yaml.
    struct MockLlm(String);
    impl mur_common::llm::LlmClient for MockLlm {
        fn complete(
            &self,
            _p: &str,
            _s: Option<&str>,
        ) -> impl std::future::Future<Output = Result<String, LlmError>> + Send {
            let v = self.0.clone();
            async move { Ok(v) }
        }
        async fn embed(&self, _t: &str) -> Result<Vec<f32>, LlmError> {
            Ok(vec![])
        }
    }

    fn role() -> RoleSpec {
        RoleSpec {
            name: "pm".into(),
            display_name: "PM".into(),
            charter: "c".into(),
            risk: RiskLevel::Low,
            preset_id: Some("pm".into()),
        }
    }

    fn valid_yaml(name: &str) -> String {
        format!(
            "name: {name}\nversion: 1.0.0\npublisher: human:pm\n\
description: A test skill for {name} used when doing {name} work in the repo.\n\
category: context\nhosts: [mur-agent]\npriority: normal\ntags: [pm]\n\
triggers:\n  - type: session_start\n  - type: command\n    pattern: /{name}\n\
content:\n  abstract: A test abstract.\n  context: |\n    # {name}\n    - Do the thing. *Why: it helps.*\n"
        )
    }

    #[tokio::test]
    async fn authors_one_skill_per_topic_and_strips_fences() {
        let fenced = format!("```yaml\n{}\n```", valid_yaml("product-spec"));
        let llm = MockLlm(fenced);
        let skills = author_skills_llm(&llm, &role(), &["product-spec".into()], &[])
            .await
            .unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "product-spec");
        assert!(skills[0].yaml.starts_with("name: product-spec"));
        // and it's valid:
        let m = mur_common::skill::parse_canonical(&skills[0].yaml).unwrap();
        assert!(mur_common::skill::validate(&m).is_ok());
    }

    #[tokio::test]
    async fn drafts_a_prompt_containing_role_and_honesty_rule() {
        let body = "# PM — c\n\n## Honesty\nyou must never fabricate command or file output.\n";
        let llm = MockLlm(body.to_string());
        let md = draft_prompt_llm(&llm, &role()).await.unwrap();
        assert!(md.contains("PM"));
        assert!(md.to_lowercase().contains("never fabricate"));
    }

    #[tokio::test]
    async fn appends_honesty_rule_when_model_omits_it() {
        let body = "# PM — c\n\nDo the work well.\n";
        let llm = MockLlm(body.to_string());
        let md = draft_prompt_llm(&llm, &role()).await.unwrap();
        assert!(md.to_lowercase().contains("never fabricate"));
    }
}
