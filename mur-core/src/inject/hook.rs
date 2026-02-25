//! Hook/inject: format patterns and workflows for injection into AI tool prompts.
//!
//! ## Post-session feedback integration
//!
//! After injection, we write `~/.mur/last_injection.json` so that
//! `mur feedback auto` can analyze the session transcript and update
//! pattern confidence. Claude Code hooks can trigger this automatically:
//!
//! ```json
//! // .claude/hooks.json (future integration)
//! {
//!   "post_session": [{
//!     "command": "mur feedback auto --file /path/to/transcript"
//!   }]
//! }
//! ```
//!
// TODO(phase-2): Integrate with Claude Code post-session hooks when the
// hooks API supports session-end events. For now, `mur feedback auto`
// is run manually or via shell scripts.

use mur_common::pattern::{Content, Pattern};
use mur_common::workflow::Workflow;

use crate::capture::feedback::{InjectedPatternRecord, InjectionRecord, write_injection_record};

/// When to trigger pattern retrieval
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)] // Manual variant used by CLI callers
pub enum HookTrigger {
    /// Beginning of AI session
    SessionStart,
    /// Error/failure detected in session
    OnError,
    /// Repeated attempt detected
    OnRetry,
    /// Manual: `mur inject --query "..."`
    Manual,
}

/// Detect what kind of hook trigger a message represents.
pub fn detect_trigger(message: &str) -> HookTrigger {
    let lower = message.to_lowercase();

    // Error indicators
    let error_keywords = [
        "error", "fail", "crash", "panic", "exception", "traceback",
        "segfault", "abort", "undefined", "cannot find", "not found",
        "permission denied", "timeout", "refused", "broken",
        "錯誤", "失敗", "崩潰",
    ];
    for kw in &error_keywords {
        if lower.contains(kw) {
            return HookTrigger::OnError;
        }
    }

    // Retry indicators
    let retry_keywords = [
        "again", "retry", "try again", "still not", "same error",
        "still failing", "didn't work", "not working",
        "再試", "還是不行", "一樣的問題",
    ];
    for kw in &retry_keywords {
        if lower.contains(kw) {
            return HookTrigger::OnRetry;
        }
    }

    HookTrigger::SessionStart
}

/// Format scored patterns for injection into a prompt.
/// Returns content only (no metadata), within token budget.
#[allow(dead_code)] // Used by tests and as public API
pub fn format_for_injection(patterns: &[Pattern], max_tokens: usize) -> String {
    if patterns.is_empty() {
        return String::new();
    }

    let mut output = String::from("## Relevant patterns from your learning history\n\n");
    let mut token_count = output.len() / 4; // rough estimate

    for (i, pattern) in patterns.iter().enumerate() {
        let entry = format_pattern_entry(pattern, i + 1);
        let entry_tokens = entry.len() / 4;

        if token_count + entry_tokens > max_tokens && i > 0 {
            break;
        }

        output.push_str(&entry);
        output.push('\n');
        token_count += entry_tokens;
    }

    output.trim_end().to_string()
}

fn format_pattern_entry(pattern: &Pattern, index: usize) -> String {
    let content = format_content(&pattern.content);
    format!(
        "### {}. {}\n{}\n",
        index,
        pattern.description,
        content.trim()
    )
}

/// Format a single workflow entry for injection.
pub fn format_workflow_entry(workflow: &Workflow, index: usize) -> String {
    let mut s = String::new();
    s.push_str(&format!("### {}. [Workflow] {}\n", index, workflow.description));

    // Content
    let content = format_content(&workflow.content);
    s.push_str(content.trim());
    s.push('\n');

    // Steps summary
    if !workflow.steps.is_empty() {
        s.push_str("Steps:\n");
        for step in &workflow.steps {
            s.push_str(&format!("  {}. {}", step.order, step.description));
            if let Some(cmd) = &step.command {
                s.push_str(&format!(" (`{}`)", cmd));
            }
            s.push('\n');
        }
    }

    s.push('\n');
    s
}

/// Format both patterns and workflows for unified injection.
pub fn format_unified_injection(
    patterns: &[Pattern],
    workflows: &[Workflow],
    max_tokens: usize,
) -> String {
    if patterns.is_empty() && workflows.is_empty() {
        return String::new();
    }

    let mut output = String::from("## Relevant knowledge from your learning history\n\n");
    let mut token_count = output.len() / 4;
    let mut index = 1;

    // Patterns first
    for pattern in patterns {
        let entry = format_pattern_entry(pattern, index);
        let entry_tokens = entry.len() / 4;
        if token_count + entry_tokens > max_tokens && index > 1 {
            break;
        }
        output.push_str(&entry);
        output.push('\n');
        token_count += entry_tokens;
        index += 1;
    }

    // Then workflows
    for workflow in workflows {
        let entry = format_workflow_entry(workflow, index);
        let entry_tokens = entry.len() / 4;
        if token_count + entry_tokens > max_tokens && index > 1 {
            break;
        }
        output.push_str(&entry);
        token_count += entry_tokens;
        index += 1;
    }

    output.trim_end().to_string()
}

/// Record which patterns were injected to `~/.mur/last_injection.json`.
///
/// Called after a successful injection so `mur feedback auto` can later
/// analyze the session transcript against these patterns.
pub fn record_injection(query: &str, project: &str, patterns: &[Pattern]) {
    let records: Vec<InjectedPatternRecord> = patterns
        .iter()
        .map(|p| {
            let full_text = p.content.as_text();
            let snippet = if full_text.len() > 100 {
                format!("{}...", &full_text[..100])
            } else {
                full_text
            };
            InjectedPatternRecord {
                name: p.name.clone(),
                snippet,
            }
        })
        .collect();

    let record = InjectionRecord {
        timestamp: chrono::Utc::now().to_rfc3339(),
        query: query.to_string(),
        project: project.to_string(),
        patterns: records,
    };

    // Best-effort: don't fail injection if recording fails
    if let Err(e) = write_injection_record(&record) {
        eprintln!("# Warning: failed to write injection record: {}", e);
    }
}

fn format_content(content: &Content) -> String {
    match content {
        Content::DualLayer {
            technical,
            principle,
        } => {
            let mut s = technical.clone();
            if let Some(p) = principle {
                s.push_str("\n💡 ");
                s.push_str(p);
            }
            s
        }
        Content::Plain(s) => s.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::knowledge::KnowledgeBase;
    use mur_common::pattern::*;
    use mur_common::workflow::{Step, FailureAction};

    fn make_pattern(desc: &str, content: &str) -> Pattern {
        Pattern {
            base: KnowledgeBase {
                schema: 2,
                name: "test".into(),
                description: desc.into(),
                content: Content::Plain(content.into()),
                tier: Tier::Session,
                importance: 0.5,
                confidence: 0.5,
                tags: Tags::default(),
                applies: Applies::default(),
                evidence: Evidence::default(),
                links: Links::default(),
                lifecycle: Lifecycle::default(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                ..Default::default()
            },
            attachments: vec![],
        }
    }

    fn make_workflow(desc: &str) -> Workflow {
        Workflow {
            base: KnowledgeBase {
                name: "test-wf".into(),
                description: desc.into(),
                content: Content::Plain("workflow content".into()),
                ..Default::default()
            },
            steps: vec![
                Step {
                    order: 1,
                    description: "Run tests".into(),
                    command: Some("cargo test".into()),
                    tool: Some("cargo".into()),
                    needs_approval: false,
                    on_failure: FailureAction::Abort,
                },
            ],
            variables: vec![],
            source_sessions: vec![],
            trigger: String::new(),
            tools: vec![],
            published_version: 0,
            permission: Default::default(),
        }
    }

    #[test]
    fn test_empty_patterns() {
        assert_eq!(format_for_injection(&[], 2000), "");
    }

    #[test]
    fn test_single_pattern() {
        let p = make_pattern("Use Swift Testing", "Use @Test macro");
        let result = format_for_injection(&[p], 2000);
        assert!(result.contains("Use Swift Testing"));
        assert!(result.contains("@Test macro"));
    }

    #[test]
    fn test_dual_layer() {
        let p = Pattern {
            base: KnowledgeBase {
                schema: 2,
                name: "test".into(),
                description: "Test".into(),
                content: Content::DualLayer {
                    technical: "Do X".into(),
                    principle: Some("Because Y".into()),
                },
                tier: Tier::Session,
                importance: 0.5,
                confidence: 0.5,
                tags: Tags::default(),
                applies: Applies::default(),
                evidence: Evidence::default(),
                links: Links::default(),
                lifecycle: Lifecycle::default(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                ..Default::default()
            },
            attachments: vec![],
        };
        let result = format_for_injection(&[p], 2000);
        assert!(result.contains("Do X"));
        assert!(result.contains("💡 Because Y"));
    }

    #[test]
    fn test_detect_error_trigger() {
        assert_eq!(detect_trigger("I got an error: cannot find module"), HookTrigger::OnError);
        assert_eq!(detect_trigger("Build failed with exit code 1"), HookTrigger::OnError);
        assert_eq!(detect_trigger("panic at thread main"), HookTrigger::OnError);
        assert_eq!(detect_trigger("程式崩潰了"), HookTrigger::OnError);
    }

    #[test]
    fn test_detect_retry_trigger() {
        assert_eq!(detect_trigger("try again please"), HookTrigger::OnRetry);
        assert_eq!(detect_trigger("retry the build"), HookTrigger::OnRetry);
        assert_eq!(detect_trigger("還是不行"), HookTrigger::OnRetry);
    }

    #[test]
    fn test_detect_session_start() {
        assert_eq!(detect_trigger("Build a REST API for users"), HookTrigger::SessionStart);
        assert_eq!(detect_trigger("Refactor the auth module"), HookTrigger::SessionStart);
    }

    #[test]
    fn test_token_budget() {
        let patterns: Vec<Pattern> = (0..20)
            .map(|i| make_pattern(&format!("Pattern {}", i), &"x".repeat(500)))
            .collect();
        let result = format_for_injection(&patterns, 500);
        // Should not include all 20 patterns
        let count = result.matches("###").count();
        assert!(count < 20);
    }

    #[test]
    fn test_format_workflow_entry() {
        let wf = make_workflow("Deploy to production");
        let entry = format_workflow_entry(&wf, 1);
        assert!(entry.contains("[Workflow]"));
        assert!(entry.contains("Deploy to production"));
        assert!(entry.contains("cargo test"));
        assert!(entry.contains("Run tests"));
    }

    #[test]
    fn test_unified_injection() {
        let patterns = vec![make_pattern("Use testing", "Use @Test macro")];
        let workflows = vec![make_workflow("Deploy flow")];
        let result = format_unified_injection(&patterns, &workflows, 5000);
        assert!(result.contains("Relevant knowledge"));
        assert!(result.contains("Use testing"));
        assert!(result.contains("[Workflow]"));
        assert!(result.contains("Deploy flow"));
    }

    #[test]
    fn test_unified_injection_empty() {
        let result = format_unified_injection(&[], &[], 5000);
        assert_eq!(result, "");
    }
}
