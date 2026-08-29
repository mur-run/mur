//! Built-in `recall` tool: let an agent read its own memories.
//!
//! Closes the half of the memory story that injection cannot. Notes reach the
//! prompt (#1049) but the block is bounded by `memory.max_in_prompt` /
//! `memory.max_chars`, so an agent with more memories than fit had no way to
//! see the rest — and the tool whose name matched the job, MCP
//! `mur_notes_search`, reads the GLOBAL note store and structurally cannot see
//! an agent's own memories at all.
//!
//! Reads the live [`RuntimeSkills`] snapshot rather than the disk. That is the
//! point: the snapshot is exactly what the prompt was built from, so `recall`
//! and the injected block can never disagree about what this agent remembers.
//! A disk read could, and two answers to one question is the failure this
//! whole area keeps producing.

use std::sync::Arc;

use super::{ToolError, ToolExecutor, ToolOutput};
use crate::llm::ToolDef;
use crate::skills::RuntimeSkills;

pub const RECALL: &str = "recall";

pub struct RecallTool {
    pub skills: Arc<RuntimeSkills>,
}

/// `recall` never needs an approval gate.
///
/// It is a pure read of this agent's own in-memory snapshot — no filesystem, no
/// network, no spend, no side effect — and it cannot surface anything the
/// injector would not have injected given more budget. Gating it gates the
/// agent's own context, and the default policy is `Ask`, so on any path without
/// a human to answer (`mur agent send`, a cron fire, a fleet step) the call
/// parks for `hitl.timeout_secs` and then fails.
///
/// Same shape as `suggest_replies_allowed`: registration is already gated —
/// here by `memory.capture` — so the tool policy has nothing left to protect.
/// An explicit `deny` rule still wins; this only changes the default.
pub fn recall_needs_no_approval(name: &str) -> bool {
    name == RECALL
}

#[async_trait::async_trait]
impl ToolExecutor for RecallTool {
    fn name(&self) -> &str {
        RECALL
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: RECALL.into(),
            description: "List what you remember about this user — the durable preferences \
                and facts you saved with `remember`, plus any shared notes. Use when asked \
                what you know or remember, or to check before saving something again."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Optional keyword filter. Omit to list everything."
                    }
                }
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let query = input
            .get("query")
            .and_then(|v| v.as_str())
            .map(str::to_lowercase);

        let snap = self.skills.snapshot();
        let mut rows: Vec<String> = snap
            .loaded
            .iter()
            .filter_map(|s| {
                let kind = mur_common::skill::lifecycle::note_kind(&s.manifest)?;
                let body = s
                    .manifest
                    .content
                    .note
                    .as_deref()
                    .unwrap_or(&s.manifest.content.r#abstract);
                // Match on the text the user would search for — name, one-line
                // description, and body — rather than the name alone, which is
                // a slug and rarely what they remember.
                if let Some(q) = &query
                    && !s.name.to_lowercase().contains(q)
                    && !s.manifest.description.to_lowercase().contains(q)
                    && !body.to_lowercase().contains(q)
                {
                    return None;
                }
                let scope = match s.scope {
                    mur_common::skill::loader::SkillScope::Agent => "mine",
                    mur_common::skill::loader::SkillScope::Global => "shared",
                };
                Some(format!(
                    "- [{}, {}] {}: {}",
                    scope,
                    match kind {
                        mur_common::skill::lifecycle::NoteKind::Rule => "rule",
                        mur_common::skill::lifecycle::NoteKind::Fact => "fact",
                    },
                    s.name,
                    body.trim()
                ))
            })
            .collect();
        rows.sort();

        if rows.is_empty() {
            return Ok(match &query {
                Some(q) => format!("nothing remembered matching '{q}'."),
                None => "nothing remembered yet.".to_string(),
            }
            .into());
        }
        Ok(format!("{} remembered:\n{}", rows.len(), rows.join("\n")).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::skill::loader::{LoadedSkill, SkillScope};
    use mur_common::skill::note::{NoteSpec, note_manifest};
    use mur_common::skill::types::TrustLevel;

    fn note(name: &str, body: &str, scope: SkillScope) -> LoadedSkill {
        LoadedSkill {
            name: name.into(),
            manifest: note_manifest(&NoteSpec {
                name,
                description: "d",
                body,
                kind: mur_common::skill::lifecycle::NoteKind::Rule,
                publisher: "agent:a1",
            }),
            trust: TrustLevel::Sandboxed,
            scope,
            content_hash: String::new(),
            dir: std::path::PathBuf::new(),
        }
    }

    fn tool(loaded: Vec<LoadedSkill>) -> RecallTool {
        RecallTool {
            skills: Arc::new(RuntimeSkills::build(loaded)),
        }
    }

    /// The default is `Ask`, so without this exemption every path with no human
    /// to answer — `mur agent send`, a cron fire, a fleet step — parks for
    /// `hitl.timeout_secs` and then fails. `recall` has nothing for a gate to
    /// protect: it reads the snapshot the prompt was already built from.
    #[test]
    fn recall_is_exempt_but_only_by_name() {
        assert!(recall_needs_no_approval(RECALL));
        assert!(!recall_needs_no_approval("remember"));
        assert!(!recall_needs_no_approval("bash"));
        assert!(!recall_needs_no_approval("recall_all"));
    }

    /// The gap this closes: an agent could not read its OWN memories. The MCP
    /// note search reads the global store and structurally cannot see them.
    #[tokio::test]
    async fn recall_returns_agent_local_memories_labelled_by_scope() {
        let t = tool(vec![
            note("reply-in-zh-tw", "always reply in zh-TW", SkillScope::Agent),
            note("house-style", "two-space indent", SkillScope::Global),
        ]);
        let out = format!("{:?}", t.execute(serde_json::json!({})).await.unwrap());
        assert!(out.contains("always reply in zh-TW"), "{out}");
        assert!(out.contains("mine"), "agent-local must be labelled: {out}");
        assert!(out.contains("shared"), "global must be labelled: {out}");
    }

    /// Non-notes share the snapshot and must not be reported as memories.
    #[tokio::test]
    async fn recall_ignores_skills_that_are_not_notes() {
        let mut skill = note("a-skill", "body", SkillScope::Global);
        skill.manifest.category = mur_common::skill::types::Category::Context;
        let t = tool(vec![skill]);
        let out = format!("{:?}", t.execute(serde_json::json!({})).await.unwrap());
        assert!(out.contains("nothing remembered"), "{out}");
    }

    /// The filter matches the body, not just the slug — a user remembers what
    /// the note SAYS, rarely what it is called.
    #[tokio::test]
    async fn query_matches_the_body_not_only_the_name() {
        let t = tool(vec![note(
            "n1",
            "the cat is called Mochi",
            SkillScope::Agent,
        )]);
        let hit = format!(
            "{:?}",
            t.execute(serde_json::json!({"query": "mochi"}))
                .await
                .unwrap()
        );
        assert!(hit.contains("Mochi"), "body match failed: {hit}");
        let miss = format!(
            "{:?}",
            t.execute(serde_json::json!({"query": "penguin"}))
                .await
                .unwrap()
        );
        assert!(miss.contains("nothing remembered matching"), "{miss}");
    }
}
