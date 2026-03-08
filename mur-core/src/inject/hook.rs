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

use mur_common::pattern::{Attachment, Content, Pattern, PatternKind};
use mur_common::workflow::Workflow;

use crate::capture::feedback::{InjectedPatternRecord, InjectionRecord, write_injection_record};
use crate::evolve::cooccurrence::CooccurrenceMatrix;
use crate::store::yaml::YamlStore;

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
        "error",
        "fail",
        "crash",
        "panic",
        "exception",
        "traceback",
        "segfault",
        "abort",
        "undefined",
        "cannot find",
        "not found",
        "permission denied",
        "timeout",
        "refused",
        "broken",
        "錯誤",
        "失敗",
        "崩潰",
    ];
    for kw in &error_keywords {
        if lower.contains(kw) {
            return HookTrigger::OnError;
        }
    }

    // Retry indicators
    let retry_keywords = [
        "again",
        "retry",
        "try again",
        "still not",
        "same error",
        "still failing",
        "didn't work",
        "not working",
        "再試",
        "還是不行",
        "一樣的問題",
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
    format_for_injection_with_store(patterns, max_tokens, None)
}

/// Format scored patterns with optional YamlStore for resolving diagram attachments.
/// Groups patterns by kind when mixed kinds are present.
pub fn format_for_injection_with_store(
    patterns: &[Pattern],
    max_tokens: usize,
    store: Option<&YamlStore>,
) -> String {
    if patterns.is_empty() {
        return String::new();
    }

    // Use flat (backward-compatible) format when every pattern has no explicit
    // kind set OR is explicitly Technical.  Fact, Procedure, Preference, and
    // Behavioral patterns trigger grouped output.
    let all_technical_or_unset = patterns
        .iter()
        .all(|p| p.kind.is_none() || p.kind == Some(PatternKind::Technical));

    if all_technical_or_unset {
        return format_flat_injection(patterns, max_tokens, store);
    }

    // Group patterns by kind category
    format_grouped_injection(patterns, max_tokens, store)
}

/// Original flat format — used when all patterns are Technical/None.
fn format_flat_injection(
    patterns: &[Pattern],
    max_tokens: usize,
    store: Option<&YamlStore>,
) -> String {
    let mut output = String::from("## Relevant patterns from your learning history\n\n");
    let mut token_count = output.len() / 4;

    for (i, pattern) in patterns.iter().enumerate() {
        let entry = format_pattern_entry(pattern, i + 1, store);
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

/// Kind category for grouping injection output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum KindGroup {
    Preferences,
    Procedures,
    Knowledge,
}

impl KindGroup {
    fn from_kind(kind: PatternKind) -> Self {
        match kind {
            PatternKind::Preference | PatternKind::Behavioral => KindGroup::Preferences,
            PatternKind::Procedure => KindGroup::Procedures,
            PatternKind::Technical | PatternKind::Fact => KindGroup::Knowledge,
        }
    }

    fn header(&self) -> &'static str {
        match self {
            KindGroup::Preferences => "## User Preferences",
            KindGroup::Procedures => "## Procedures",
            KindGroup::Knowledge => "## Knowledge",
        }
    }
}

/// Grouped format — patterns grouped by kind with appropriate formatting.
fn format_grouped_injection(
    patterns: &[Pattern],
    max_tokens: usize,
    store: Option<&YamlStore>,
) -> String {
    let mut output = String::from("## Relevant knowledge from your learning history\n\n");
    let mut token_count = output.len() / 4;

    // Group patterns maintaining relative order within groups
    let group_order = [
        KindGroup::Preferences,
        KindGroup::Procedures,
        KindGroup::Knowledge,
    ];
    let mut index = 1;

    for group in &group_order {
        let group_patterns: Vec<&Pattern> = patterns
            .iter()
            .filter(|p| KindGroup::from_kind(p.effective_kind()) == *group)
            .collect();

        if group_patterns.is_empty() {
            continue;
        }

        let header = format!("{}\n\n", group.header());
        let header_tokens = header.len() / 4;
        if token_count + header_tokens > max_tokens && index > 1 {
            break;
        }
        output.push_str(&header);
        token_count += header_tokens;

        for pattern in group_patterns {
            let entry = match group {
                KindGroup::Preferences => format_preference_entry(pattern),
                KindGroup::Procedures => format_procedure_entry(pattern),
                KindGroup::Knowledge => format_pattern_entry(pattern, index, store),
            };
            let entry_tokens = entry.len() / 4;

            if token_count + entry_tokens > max_tokens && index > 1 {
                break;
            }

            output.push_str(&entry);
            output.push('\n');
            token_count += entry_tokens;
            index += 1;
        }
    }

    output.trim_end().to_string()
}

/// Format a preference pattern as a bullet point.
fn format_preference_entry(pattern: &Pattern) -> String {
    let content = format_content(&pattern.content);
    format!("- **{}**: {}\n", pattern.description, content.trim())
}

/// Format a procedure pattern as numbered steps.
fn format_procedure_entry(pattern: &Pattern) -> String {
    let content = format_content(&pattern.content);
    format!("### {}\n{}\n", pattern.description, content.trim())
}

fn format_pattern_entry(pattern: &Pattern, index: usize, store: Option<&YamlStore>) -> String {
    let content = format_content(&pattern.content);
    let mut entry = format!(
        "### {}. {}\n{}\n",
        index,
        pattern.description,
        content.trim()
    );

    // Inline diagram attachments
    for attachment in &pattern.attachments {
        entry.push_str(&format_attachment_for_injection(attachment, store));
    }

    entry
}

/// Format an attachment for injection into a prompt.
/// Text-based diagrams are inlined; images get description only.
fn format_attachment_for_injection(attachment: &Attachment, store: Option<&YamlStore>) -> String {
    if attachment.format.is_text_based() {
        // Try to resolve and inline the diagram content
        let content = store.and_then(|s| s.resolve_attachment_content(attachment));
        if let Some(diagram) = content {
            return format!(
                "\n## Diagram: {}\n```{}\n{}\n```\n",
                attachment.description,
                attachment.format.fence_lang(),
                diagram.trim()
            );
        }
        // Couldn't resolve — fall back to description only
        format!(
            "\n📎 Diagram: {} ({})\n",
            attachment.description, attachment.path
        )
    } else {
        // Binary attachment — description only
        format!("\n📎 {}: {}\n", attachment.description, attachment.path)
    }
}

/// Format a single workflow entry for injection.
pub fn format_workflow_entry(workflow: &Workflow, index: usize) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "### {}. [Workflow: {}] {}\n",
        index, workflow.name, workflow.description
    ));
    s.push_str("**Follow this workflow if the task matches.** ");
    s.push_str("Run `mur workflow show ");
    s.push_str(&workflow.name);
    s.push_str(" --md` for full details.\n");

    // Variables
    if !workflow.variables.is_empty() {
        s.push_str("Variables: ");
        let vars: Vec<String> = workflow.variables.iter().map(|v| {
            let default = v.default_value.as_deref().unwrap_or("?");
            format!("`{}`={}", v.name, default)
        }).collect();
        s.push_str(&vars.join(", "));
        s.push('\n');
    }

    // Tools
    if !workflow.tools.is_empty() {
        s.push_str(&format!("Tools: {}\n", workflow.tools.join(", ")));
    }

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
#[allow(dead_code)] // Used by tests and as public API
pub fn format_unified_injection(
    patterns: &[Pattern],
    workflows: &[Workflow],
    max_tokens: usize,
) -> String {
    format_unified_injection_with_store(patterns, workflows, max_tokens, None)
}

/// Format both patterns and workflows with optional store for diagram resolution.
pub fn format_unified_injection_with_store(
    patterns: &[Pattern],
    workflows: &[Workflow],
    max_tokens: usize,
    store: Option<&YamlStore>,
) -> String {
    if patterns.is_empty() && workflows.is_empty() {
        return String::new();
    }

    let mut output = String::from("## Relevant knowledge from your learning history\n\n");
    let mut token_count = output.len() / 4;
    let mut index = 1;

    // Patterns first
    for pattern in patterns {
        let entry = format_pattern_entry(pattern, index, store);
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
                let end = full_text
                    .char_indices()
                    .take_while(|(i, _)| *i <= 100)
                    .last()
                    .map(|(i, c)| i + c.len_utf8())
                    .unwrap_or(full_text.len().min(100));
                format!("{}...", &full_text[..end])
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

/// Record co-occurrence of injected patterns to `~/.mur/cooccurrence.json`.
///
/// Called after a successful injection so the co-occurrence matrix tracks
/// which patterns appear together in the same session.
pub fn record_cooccurrence_for_injection(patterns: &[Pattern]) {
    if patterns.len() < 2 {
        return;
    }

    let path = CooccurrenceMatrix::default_path();
    let mut matrix = match CooccurrenceMatrix::load(&path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("# Warning: failed to load cooccurrence matrix: {}", e);
            CooccurrenceMatrix::new()
        }
    };

    let names: Vec<String> = patterns.iter().map(|p| p.name.clone()).collect();
    matrix.record_cooccurrence(&names);

    if let Err(e) = matrix.save(&path) {
        eprintln!("# Warning: failed to save cooccurrence matrix: {}", e);
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
    use mur_common::workflow::{FailureAction, Step};

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
            kind: None,
            origin: None,
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
            steps: vec![Step {
                order: 1,
                description: "Run tests".into(),
                command: Some("cargo test".into()),
                tool: Some("cargo".into()),
                needs_approval: false,
                on_failure: FailureAction::Abort,
            }],
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
            kind: None,
            origin: None,
            attachments: vec![],
        };
        let result = format_for_injection(&[p], 2000);
        assert!(result.contains("Do X"));
        assert!(result.contains("💡 Because Y"));
    }

    #[test]
    fn test_detect_error_trigger() {
        assert_eq!(
            detect_trigger("I got an error: cannot find module"),
            HookTrigger::OnError
        );
        assert_eq!(
            detect_trigger("Build failed with exit code 1"),
            HookTrigger::OnError
        );
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
        assert_eq!(
            detect_trigger("Build a REST API for users"),
            HookTrigger::SessionStart
        );
        assert_eq!(
            detect_trigger("Refactor the auth module"),
            HookTrigger::SessionStart
        );
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

    // ─── Phase 3: Diagram attachment injection tests ────────────

    #[test]
    fn test_injection_with_diagram_attachment_no_store() {
        let p = Pattern {
            base: KnowledgeBase {
                schema: 2,
                name: "arch-pattern".into(),
                description: "Architecture pattern".into(),
                content: Content::Plain("Use microservices.".into()),
                tier: Tier::Core,
                importance: 0.8,
                confidence: 0.9,
                ..Default::default()
            },
            kind: None,
            origin: None,
            attachments: vec![Attachment {
                att_type: AttachmentType::Diagram,
                format: AttachmentFormat::Mermaid,
                path: "arch-pattern/overview.mermaid".into(),
                description: "System architecture".into(),
            }],
        };

        // Without store, diagram can't be resolved — should show path fallback
        let result = format_for_injection(&[p], 5000);
        assert!(result.contains("Architecture pattern"));
        assert!(result.contains("Use microservices"));
        assert!(result.contains("Diagram: System architecture"));
        assert!(result.contains("arch-pattern/overview.mermaid"));
    }

    #[test]
    fn test_injection_with_diagram_attachment_with_store() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = crate::store::yaml::YamlStore::new(tmp.path().to_path_buf()).unwrap();

        // Create the diagram file
        let assets_dir = tmp.path().join("arch-pattern");
        std::fs::create_dir_all(&assets_dir).unwrap();
        std::fs::write(
            assets_dir.join("overview.mermaid"),
            "graph TD\n    A[Client]-->B[Server]\n    B-->C[Database]",
        )
        .unwrap();

        let p = Pattern {
            base: KnowledgeBase {
                schema: 2,
                name: "arch-pattern".into(),
                description: "Architecture pattern".into(),
                content: Content::Plain("Use microservices.".into()),
                ..Default::default()
            },
            kind: None,
            origin: None,
            attachments: vec![Attachment {
                att_type: AttachmentType::Diagram,
                format: AttachmentFormat::Mermaid,
                path: "arch-pattern/overview.mermaid".into(),
                description: "System architecture".into(),
            }],
        };

        let result = format_for_injection_with_store(&[p], 5000, Some(&store));
        assert!(result.contains("## Diagram: System architecture"));
        assert!(result.contains("```mermaid"));
        assert!(result.contains("A[Client]-->B[Server]"));
        assert!(result.contains("```"));
    }

    #[test]
    fn test_injection_image_attachment_description_only() {
        let p = Pattern {
            base: KnowledgeBase {
                schema: 2,
                name: "ui-pattern".into(),
                description: "UI pattern".into(),
                content: Content::Plain("Use dark theme.".into()),
                ..Default::default()
            },
            kind: None,
            origin: None,
            attachments: vec![Attachment {
                att_type: AttachmentType::Image,
                format: AttachmentFormat::Png,
                path: "ui-pattern/screenshot.png".into(),
                description: "Dark mode screenshot".into(),
            }],
        };

        let result = format_for_injection(&[p], 5000);
        assert!(result.contains("Dark mode screenshot"));
        // Should NOT contain mermaid code fence
        assert!(!result.contains("```mermaid"));
        assert!(!result.contains("```png"));
    }

    #[test]
    fn test_pattern_without_attachments_unchanged() {
        let p = make_pattern("No attachments", "Use foo bar.");
        let result = format_for_injection(&[p], 5000);
        assert!(result.contains("No attachments"));
        assert!(result.contains("Use foo bar"));
        // No attachment markers
        assert!(!result.contains("Diagram:"));
        assert!(!result.contains("📎"));
    }

    // ─── Kind-aware formatting tests ────────────────────────────

    #[test]
    fn test_mixed_kind_injection_grouped() {
        let mut p_pref = make_pattern("Prefer Chinese", "Always use Traditional Chinese");
        p_pref.kind = Some(PatternKind::Preference);

        let mut p_proc = make_pattern("Deploy steps", "1. Run tests 2. Build 3. Deploy");
        p_proc.kind = Some(PatternKind::Procedure);

        let p_tech = make_pattern("Use @Test", "Use @Test macro for Swift testing");
        // kind is None = Technical

        let result = format_for_injection(&[p_pref, p_proc, p_tech], 5000);
        assert!(
            result.contains("User Preferences"),
            "Should have Preferences header"
        );
        assert!(
            result.contains("Procedures"),
            "Should have Procedures header"
        );
        assert!(result.contains("Knowledge"), "Should have Knowledge header");
        assert!(result.contains("Traditional Chinese"));
        assert!(result.contains("Deploy steps"));
        assert!(result.contains("@Test macro"));
    }

    #[test]
    fn test_all_technical_uses_flat_format() {
        let p1 = make_pattern("Pattern A", "Content A");
        let p2 = make_pattern("Pattern B", "Content B");
        let result = format_for_injection(&[p1, p2], 5000);
        // Should use the old flat format header
        assert!(result.contains("Relevant patterns from your learning history"));
        // Should NOT have kind-group headers
        assert!(!result.contains("User Preferences"));
        assert!(!result.contains("Procedures"));
    }

    #[test]
    fn test_explicit_technical_kind_still_flat() {
        // explicit kind=Technical should still use flat format
        let mut p1 = make_pattern("Pattern A", "Content A");
        p1.kind = Some(PatternKind::Technical);
        let mut p2 = make_pattern("Pattern B", "Content B");
        p2.kind = Some(PatternKind::Technical);
        let result = format_for_injection(&[p1, p2], 5000);
        assert!(result.contains("Relevant patterns from your learning history"));
        assert!(!result.contains("Knowledge"));
    }

    #[test]
    fn test_explicit_fact_kind_triggers_grouped_format() {
        // Fact with explicit kind → grouped (even though it's in Knowledge group)
        let mut p1 = make_pattern("Server address", "prod.example.com:8080");
        p1.kind = Some(PatternKind::Fact);
        let result = format_for_injection(&[p1], 5000);
        assert!(
            result.contains("Relevant knowledge from your learning history"),
            "Explicit Fact kind should use grouped header"
        );
        assert!(result.contains("Knowledge"));
    }

    #[test]
    fn test_preferences_as_bullet_list() {
        let mut p = make_pattern("Short responses", "Keep answers concise");
        p.kind = Some(PatternKind::Preference);

        let mut p2 = make_pattern("Tech pattern", "Use Rust");
        p2.kind = Some(PatternKind::Technical);

        let result = format_for_injection(&[p, p2], 5000);
        // Preferences should be bullet points
        assert!(result.contains("- **"));
    }
}
