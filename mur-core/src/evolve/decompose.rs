//! Workflow → Pattern extraction (decomposition).
//!
//! Analyzes workflow steps to find candidates that could become
//! standalone reusable patterns. Uses heuristics to distinguish
//! generic, reusable steps from project-specific ones.

use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::{Content, Pattern};
use mur_common::workflow::Workflow;

/// A candidate step from a workflow that could become a pattern.
#[derive(Debug, Clone)]
pub struct DecompositionCandidate {
    /// Index into workflow.steps
    pub step_index: usize,
    /// The step's description
    pub step_description: String,
    /// Suggested pattern name (kebab-case)
    pub suggested_pattern_name: String,
    /// Suggested pattern content
    #[allow(dead_code)] // Used by --create flag consumers
    pub suggested_content: String,
    /// Why this step is a good candidate
    pub reason: String,
}

/// Keywords that indicate project-specific steps (not good candidates).
const PROJECT_SPECIFIC_KEYWORDS: &[&str] = &[
    "deploy to",
    "push to",
    "merge into",
    "our",
    "the team",
    "production",
    "staging",
    "specific",
    "internal",
    "company",
    "org",
    "repo",
];

/// Keywords that indicate generic, reusable steps (good candidates).
const GENERIC_KEYWORDS: &[&str] = &[
    "run tests",
    "lint",
    "format",
    "check",
    "validate",
    "build",
    "install",
    "update",
    "clean",
    "reset",
    "backup",
    "restore",
    "generate",
    "compile",
    "analyze",
    "audit",
    "verify",
    "setup",
    "configure",
    "initialize",
];

/// Analyze a workflow's steps for pattern extraction candidates.
///
/// Heuristics:
/// - Steps with generic descriptions (not project-specific) → candidate
/// - Steps that match keywords from existing patterns → already covered, skip
/// - Steps > 50 chars with actionable content → good candidate
pub fn analyze_workflow_for_extraction(
    workflow: &Workflow,
    existing_patterns: &[Pattern],
) -> Vec<DecompositionCandidate> {
    let existing_names: Vec<String> = existing_patterns.iter().map(|p| p.name.clone()).collect();
    let existing_content: Vec<String> = existing_patterns
        .iter()
        .map(|p| p.content.as_text().to_lowercase())
        .collect();

    let mut candidates = Vec::new();

    for (i, step) in workflow.steps.iter().enumerate() {
        let desc_lower = step.description.to_lowercase();

        // Skip if this step is already covered by an existing pattern
        if is_covered_by_existing(&desc_lower, &existing_names, &existing_content) {
            continue;
        }

        // Skip project-specific steps
        if is_project_specific(&desc_lower) {
            continue;
        }

        // Check if this is a good generic candidate
        let reason = evaluate_step_quality(&step.description, &step.command);

        if let Some(reason) = reason {
            let suggested_name = generate_pattern_name(&step.description, &workflow.name);
            let suggested_content = generate_pattern_content(step, workflow);

            candidates.push(DecompositionCandidate {
                step_index: i,
                step_description: step.description.clone(),
                suggested_pattern_name: suggested_name,
                suggested_content,
                reason,
            });
        }
    }

    candidates
}

/// Create a draft Pattern from a workflow step.
pub fn extract_pattern_from_step(workflow: &Workflow, step_index: usize) -> Option<Pattern> {
    let step = workflow.steps.get(step_index)?;

    let name = generate_pattern_name(&step.description, &workflow.name);
    let content = generate_pattern_content(step, workflow);

    let mut pattern = Pattern {
        base: KnowledgeBase {
            name,
            description: step.description.clone(),
            content: Content::Plain(content),
            tags: workflow.base.tags.clone(),
            applies: workflow.base.applies.clone(),
            links: mur_common::pattern::Links {
                workflows: vec![workflow.name.clone()],
                ..Default::default()
            },
            ..Default::default()
        },
        kind: None,
        origin: None,
        attachments: vec![],
    };

    // Copy tool info to tags if present
    if let Some(ref tool) = step.tool
        && !pattern.tags.topics.contains(tool)
    {
        pattern.base.tags.topics.push(tool.clone());
    }

    Some(pattern)
}

/// Check if a step description is already covered by existing patterns.
fn is_covered_by_existing(
    desc_lower: &str,
    existing_names: &[String],
    existing_content: &[String],
) -> bool {
    let desc_words: Vec<&str> = desc_lower
        .split_whitespace()
        .filter(|w| w.len() > 3)
        .collect();

    if desc_words.is_empty() {
        return false;
    }

    // Check if existing pattern names match significantly
    for name in existing_names {
        let name_words: Vec<&str> = name.split('-').collect();
        let overlap = desc_words
            .iter()
            .filter(|w| {
                name_words
                    .iter()
                    .any(|nw| nw.contains(*w) || w.contains(nw))
            })
            .count();
        if overlap >= 2 || (desc_words.len() <= 3 && overlap >= 1) {
            return true;
        }
    }

    // Check if existing pattern content matches significantly
    for content in existing_content {
        let matching_words = desc_words.iter().filter(|w| content.contains(*w)).count();
        let ratio = matching_words as f64 / desc_words.len() as f64;
        if ratio > 0.6 {
            return true;
        }
    }

    false
}

/// Check if a step description is project-specific.
fn is_project_specific(desc_lower: &str) -> bool {
    PROJECT_SPECIFIC_KEYWORDS
        .iter()
        .any(|kw| desc_lower.contains(kw))
}

/// Evaluate whether a step is a good candidate for extraction.
/// Returns Some(reason) if it is, None if not.
fn evaluate_step_quality(description: &str, command: &Option<String>) -> Option<String> {
    let desc_lower = description.to_lowercase();

    // Check for generic actionable keywords
    for kw in GENERIC_KEYWORDS {
        if desc_lower.contains(kw) {
            return Some(format!("Generic reusable step (matches '{}')", kw));
        }
    }

    // Steps with commands are more concrete and extractable
    if command.is_some() && description.len() > 20 {
        return Some("Step has concrete command and detailed description".to_string());
    }

    // Long descriptions with actionable content
    if description.len() > 50 {
        return Some("Detailed actionable step (>50 chars)".to_string());
    }

    None
}

/// Generate a kebab-case pattern name from step description.
fn generate_pattern_name(description: &str, workflow_name: &str) -> String {
    let lowered = description.to_lowercase();
    let words: Vec<&str> = lowered
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2 && !is_stop_word(w))
        .take(4)
        .collect();

    let base = if words.is_empty() {
        format!("{}-step", workflow_name)
    } else {
        words.join("-")
    };

    // Ensure it's unique-ish by including workflow context
    let wf_prefix = workflow_name.split('-').next().unwrap_or("wf");
    if base.starts_with(wf_prefix) {
        base
    } else {
        format!("{}-{}", wf_prefix, base)
    }
}

/// Common stop words to filter from pattern names.
fn is_stop_word(word: &str) -> bool {
    matches!(
        word,
        "the" | "and" | "for" | "with" | "from" | "into" | "that" | "this" | "then" | "when"
    )
}

/// Generate pattern content from a workflow step.
fn generate_pattern_content(step: &mur_common::workflow::Step, workflow: &Workflow) -> String {
    let mut content = step.description.clone();

    if let Some(ref cmd) = step.command {
        content.push_str(&format!("\n\nCommand: `{}`", cmd));
    }

    if let Some(ref tool) = step.tool {
        content.push_str(&format!("\nTool: {}", tool));
    }

    content.push_str(&format!("\n\nExtracted from workflow: {}", workflow.name));

    content
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::knowledge::KnowledgeBase;
    use mur_common::pattern::Content;
    use mur_common::workflow::{FailureAction, Step};

    fn make_workflow_with_steps(name: &str, steps: Vec<Step>) -> Workflow {
        Workflow {
            base: KnowledgeBase {
                name: name.into(),
                description: format!("Workflow: {}", name),
                content: Content::Plain("workflow content".into()),
                ..Default::default()
            },
            steps,
            variables: vec![],
            source_sessions: vec![],
            trigger: String::new(),
            tools: vec![],
            published_version: 0,
            permission: Default::default(),
        }
    }

    fn make_step(order: u32, desc: &str, cmd: Option<&str>) -> Step {
        Step {
            order,
            description: desc.into(),
            command: cmd.map(String::from),
            tool: None,
            needs_approval: false,
            on_failure: FailureAction::Abort,
        }
    }

    fn make_pattern(name: &str, content: &str) -> Pattern {
        Pattern {
            base: KnowledgeBase {
                name: name.into(),
                description: name.into(),
                content: Content::Plain(content.into()),
                ..Default::default()
            },
            kind: None,
            origin: None,
            attachments: vec![],
        }
    }

    #[test]
    fn test_analyze_generic_steps() {
        let workflow = make_workflow_with_steps(
            "deploy-flow",
            vec![
                make_step(
                    1,
                    "Run tests to verify everything works",
                    Some("cargo test"),
                ),
                make_step(2, "Deploy to our production server", None),
                make_step(3, "Build the release binary", Some("cargo build --release")),
            ],
        );

        let candidates = analyze_workflow_for_extraction(&workflow, &[]);
        // Step 1 (run tests) and step 3 (build) should be candidates
        // Step 2 (deploy to our production) is project-specific
        assert!(candidates.len() >= 2);
        let descs: Vec<&str> = candidates
            .iter()
            .map(|c| c.step_description.as_str())
            .collect();
        assert!(descs.iter().any(|d| d.contains("Run tests")));
        assert!(descs.iter().any(|d| d.contains("Build")));
        assert!(!descs.iter().any(|d| d.contains("Deploy to our")));
    }

    #[test]
    fn test_skip_already_covered_steps() {
        let workflow = make_workflow_with_steps(
            "ci-flow",
            vec![
                make_step(1, "Run cargo tests", Some("cargo test")),
                make_step(2, "Lint the code with clippy", Some("cargo clippy")),
            ],
        );

        let existing = vec![make_pattern(
            "cargo-test-usage",
            "Always run cargo test before committing",
        )];

        let candidates = analyze_workflow_for_extraction(&workflow, &existing);
        // Step 1 should be skipped because "cargo-test" matches
        // Step 2 should be a candidate
        let descs: Vec<&str> = candidates
            .iter()
            .map(|c| c.step_description.as_str())
            .collect();
        assert!(!descs.iter().any(|d| d.contains("cargo tests")));
        assert!(descs.iter().any(|d| d.contains("Lint")));
    }

    #[test]
    fn test_project_specific_detection() {
        assert!(is_project_specific("deploy to staging"));
        assert!(is_project_specific("merge into our main branch"));
        assert!(is_project_specific("push to production"));
        assert!(!is_project_specific("run cargo test"));
        assert!(!is_project_specific("build the binary"));
    }

    #[test]
    fn test_extract_pattern_from_step() {
        let workflow = make_workflow_with_steps(
            "ci-flow",
            vec![make_step(1, "Run lint checks", Some("cargo clippy"))],
        );

        let pattern = extract_pattern_from_step(&workflow, 0).unwrap();
        assert!(!pattern.name.is_empty());
        assert_eq!(pattern.description, "Run lint checks");
        assert!(pattern.content.as_text().contains("cargo clippy"));
        assert!(pattern.links.workflows.contains(&"ci-flow".to_string()));
    }

    #[test]
    fn test_extract_pattern_invalid_index() {
        let workflow = make_workflow_with_steps("wf", vec![]);
        assert!(extract_pattern_from_step(&workflow, 0).is_none());
    }

    #[test]
    fn test_long_description_candidate() {
        let workflow = make_workflow_with_steps(
            "setup-flow",
            vec![make_step(
                1,
                "Ensure all environment variables are properly set before starting the application server process",
                None,
            )],
        );

        let candidates = analyze_workflow_for_extraction(&workflow, &[]);
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].reason.contains(">50 chars"));
    }

    #[test]
    fn test_generate_pattern_name() {
        let name = generate_pattern_name("Run cargo tests and verify", "deploy-flow");
        assert!(name.contains("cargo"));
        assert!(name.contains("tests"));
    }
}
