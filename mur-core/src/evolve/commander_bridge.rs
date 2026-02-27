//! Commander bridge — converts evolved patterns into Commander workflow YAML.
//!
//! After pattern evolution (maturity, lifecycle), this module detects patterns
//! that are good candidates for automation and generates Commander-compatible
//! workflow YAML files.

use anyhow::{Context, Result};
use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::{Content, Pattern};
use mur_common::knowledge::Maturity;
use mur_common::workflow::{FailureAction, Permission, Step, Workflow};
use std::fs;
use std::path::{Path, PathBuf};

/// Configuration for the Commander bridge.
#[derive(Debug, Clone)]
pub struct CommanderBridgeConfig {
    /// Directory to write Commander workflow YAML files.
    pub workflows_dir: PathBuf,
    /// Whether to automatically suggest workflows after pattern evolution.
    pub auto_suggest: bool,
}

impl Default for CommanderBridgeConfig {
    fn default() -> Self {
        let workflows_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("~"))
            .join(".mur")
            .join("workflows");
        Self {
            workflows_dir,
            auto_suggest: true,
        }
    }
}

/// Bridge between evolved patterns and Commander workflows.
#[derive(Debug, Clone)]
pub struct CommanderBridge {
    pub config: CommanderBridgeConfig,
}

/// A pattern detected as a candidate for workflow generation.
#[derive(Debug, Clone)]
pub struct WorkflowCandidate {
    /// Name of the source pattern.
    pub pattern_name: String,
    /// Why this pattern is a candidate.
    pub reason: String,
    /// Confidence that this is a good workflow candidate (0.0–1.0).
    pub confidence: f64,
}

/// Preview of a generated workflow before saving.
#[derive(Debug, Clone)]
pub struct WorkflowPreview {
    /// The candidate that triggered this preview.
    pub candidate: WorkflowCandidate,
    /// Serialized YAML content.
    pub yaml_content: String,
    /// The generated workflow struct.
    pub workflow: Workflow,
}

/// Keywords that signal a pattern contains automatable actions.
const ACTION_KEYWORDS: &[&str] = &[
    "run ",
    "execute",
    "build",
    "test",
    "deploy",
    "install",
    "compile",
    "lint",
    "format",
    "check",
    "cargo ",
    "npm ",
    "make ",
    "docker ",
    "git ",
    "curl ",
    "mkdir ",
];

impl CommanderBridge {
    /// Create a new Commander bridge with the given config.
    pub fn new(config: CommanderBridgeConfig) -> Self {
        Self { config }
    }

    /// Create a Commander bridge with default config.
    pub fn with_defaults() -> Self {
        Self::new(CommanderBridgeConfig::default())
    }

    /// Scan patterns for automation candidates.
    ///
    /// A pattern is a candidate when:
    /// - Maturity is Stable or Canonical
    /// - Evidence effectiveness >= 0.6
    /// - Content contains actionable keywords (commands, tool references)
    pub fn detect_workflow_candidates(&self, patterns: &[Pattern]) -> Vec<WorkflowCandidate> {
        patterns
            .iter()
            .filter_map(|p| self.evaluate_candidate(p))
            .collect()
    }

    /// Convert a pattern into a Commander workflow.
    pub fn pattern_to_commander_yaml(&self, pattern: &Pattern) -> Result<String> {
        let workflow = self.build_workflow(pattern);
        serde_yaml::to_string(&workflow).context("Failed to serialize workflow to YAML")
    }

    /// Present a suggestion with a preview of the generated workflow.
    pub fn suggest_workflow(&self, pattern: &Pattern) -> Result<Option<WorkflowPreview>> {
        let candidate = match self.evaluate_candidate(pattern) {
            Some(c) => c,
            None => return Ok(None),
        };
        let workflow = self.build_workflow(pattern);
        let yaml_content =
            serde_yaml::to_string(&workflow).context("Failed to serialize workflow to YAML")?;
        Ok(Some(WorkflowPreview {
            candidate,
            yaml_content,
            workflow,
        }))
    }

    /// Write a workflow YAML file to the workflows directory.
    pub fn save_workflow(&self, workflow: &Workflow) -> Result<PathBuf> {
        fs::create_dir_all(&self.config.workflows_dir).with_context(|| {
            format!(
                "Failed to create workflows dir: {}",
                self.config.workflows_dir.display()
            )
        })?;

        let path = self
            .config
            .workflows_dir
            .join(format!("{}.yaml", workflow.name));
        let yaml =
            serde_yaml::to_string(workflow).context("Failed to serialize workflow to YAML")?;

        // Atomic write: temp file → rename
        let tmp_path = path.with_extension("yaml.tmp");
        fs::write(&tmp_path, &yaml)
            .with_context(|| format!("Failed to write temp file: {}", tmp_path.display()))?;
        fs::rename(&tmp_path, &path)
            .with_context(|| format!("Failed to rename temp to final: {}", path.display()))?;

        Ok(path)
    }

    /// Evaluate whether a single pattern is a workflow candidate.
    fn evaluate_candidate(&self, pattern: &Pattern) -> Option<WorkflowCandidate> {
        // Must be Stable or Canonical maturity
        let mature = matches!(pattern.maturity, Maturity::Stable | Maturity::Canonical);
        if !mature {
            return None;
        }

        // Must have reasonable effectiveness
        let effectiveness = pattern.evidence.effectiveness();
        if effectiveness < 0.6 {
            return None;
        }

        // Content must contain actionable keywords
        let content_text = extract_content_text(&pattern.base);
        let content_lower = content_text.to_lowercase();
        let action_count = ACTION_KEYWORDS
            .iter()
            .filter(|kw| content_lower.contains(*kw))
            .count();
        if action_count == 0 {
            return None;
        }

        // Build reason and confidence
        let mut reasons = Vec::new();
        reasons.push(format!("maturity={:?}", pattern.maturity));
        reasons.push(format!("effectiveness={:.0}%", effectiveness * 100.0));
        reasons.push(format!("{} action keywords detected", action_count));

        if !pattern.applies.tools.is_empty() {
            reasons.push(format!("tools: {}", pattern.applies.tools.join(", ")));
        }

        // Confidence: base from effectiveness, boost from action density and maturity
        let maturity_boost = if matches!(pattern.maturity, Maturity::Canonical) {
            0.1
        } else {
            0.0
        };
        let action_boost = (action_count as f64 * 0.05).min(0.2);
        let confidence = (effectiveness * 0.7 + action_boost + maturity_boost).min(1.0);

        Some(WorkflowCandidate {
            pattern_name: pattern.name.clone(),
            reason: reasons.join("; "),
            confidence,
        })
    }

    /// Build a Workflow struct from a pattern.
    fn build_workflow(&self, pattern: &Pattern) -> Workflow {
        let content_text = extract_content_text(&pattern.base);
        let steps = extract_steps(&content_text, pattern);
        let tools = collect_tools(pattern, &steps);

        let trigger = if !pattern.tags.topics.is_empty() {
            format!(
                "when working with {}",
                pattern.tags.topics.join(" and ")
            )
        } else {
            format!("when applying {}", pattern.name)
        };

        let workflow_name = format!("cmd-{}", pattern.name);

        Workflow {
            base: KnowledgeBase {
                name: workflow_name,
                description: format!(
                    "Auto-generated Commander workflow from pattern '{}'",
                    pattern.name
                ),
                content: pattern.content.clone(),
                tier: pattern.tier.clone(),
                tags: pattern.tags.clone(),
                applies: pattern.applies.clone(),
                ..Default::default()
            },
            steps,
            variables: vec![],
            source_sessions: vec![],
            trigger,
            tools,
            published_version: 0,
            permission: Permission::Read,
        }
    }
}

/// Extract plain text from pattern content (handles DualLayer and Plain).
fn extract_content_text(base: &KnowledgeBase) -> String {
    match &base.content {
        Content::DualLayer {
            technical,
            principle,
        } => {
            let mut text = technical.clone();
            if let Some(p) = principle {
                text.push('\n');
                text.push_str(p);
            }
            text
        }
        Content::Plain(s) => s.clone(),
    }
}

/// Extract steps from pattern content by detecting command-like lines.
fn extract_steps(content: &str, pattern: &Pattern) -> Vec<Step> {
    let mut steps = Vec::new();
    let mut order = 1u32;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Detect lines that look like shell commands
        let is_command = trimmed.starts_with("$ ")
            || trimmed.starts_with("```")
            || ACTION_KEYWORDS
                .iter()
                .any(|kw| trimmed.to_lowercase().starts_with(kw));

        if is_command {
            let (description, command) = parse_command_line(trimmed);
            let tool = infer_tool(&command, pattern);

            steps.push(Step {
                order,
                description,
                command: Some(command),
                tool,
                needs_approval: false,
                on_failure: FailureAction::Abort,
            });
            order += 1;
        }
    }

    // If no commands were found, create a single step from the description
    if steps.is_empty() {
        steps.push(Step {
            order: 1,
            description: pattern.description.clone(),
            command: None,
            tool: pattern.applies.tools.first().cloned(),
            needs_approval: true,
            on_failure: FailureAction::Abort,
        });
    }

    steps
}

/// Parse a command-like line into (description, command).
fn parse_command_line(line: &str) -> (String, String) {
    // "$ cargo build" → ("cargo build", "cargo build")
    if let Some(cmd) = line.strip_prefix("$ ") {
        let cmd = cmd.trim().to_string();
        (cmd.clone(), cmd)
    } else {
        (line.to_string(), line.to_string())
    }
}

/// Infer the tool name from a command string.
fn infer_tool(command: &str, pattern: &Pattern) -> Option<String> {
    let first_word = command.split_whitespace().next().unwrap_or("");
    let known_tools = [
        "cargo", "npm", "npx", "yarn", "pnpm", "docker", "git", "make", "bash", "sh", "python",
        "pip", "go", "rustup", "kubectl", "curl", "wget",
    ];

    if known_tools.contains(&first_word) {
        return Some(first_word.to_string());
    }

    // Fall back to pattern's tool list
    pattern.applies.tools.first().cloned()
}

/// Collect unique tools from pattern metadata and extracted steps.
fn collect_tools(pattern: &Pattern, steps: &[Step]) -> Vec<String> {
    let mut tools: Vec<String> = pattern.applies.tools.clone();
    for step in steps {
        if let Some(ref t) = step.tool {
            if !tools.contains(t) {
                tools.push(t.clone());
            }
        }
    }
    tools
}

/// Check if a workflow file already exists at the given path.
pub fn workflow_exists(workflows_dir: &Path, pattern_name: &str) -> bool {
    let path = workflows_dir.join(format!("cmd-{}.yaml", pattern_name));
    path.exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::knowledge::{Evidence, Links};
    use mur_common::pattern::Tags;
    use tempfile::TempDir;

    fn make_automatable_pattern(name: &str) -> Pattern {
        Pattern {
            base: KnowledgeBase {
                name: name.to_string(),
                description: format!("Pattern for {}", name),
                content: Content::Plain(
                    "$ cargo build --release\n$ cargo test\n$ cargo clippy".to_string(),
                ),
                maturity: Maturity::Stable,
                tags: Tags {
                    topics: vec!["ci".into(), "quality".into()],
                    languages: vec!["rust".into()],
                    extra: Default::default(),
                },
                evidence: Evidence {
                    injection_count: 20,
                    success_signals: 18,
                    override_signals: 2,
                    ..Default::default()
                },
                links: Links::default(),
                ..Default::default()
            },
            attachments: vec![],
        }
    }

    fn make_non_automatable_pattern(name: &str) -> Pattern {
        Pattern {
            base: KnowledgeBase {
                name: name.to_string(),
                description: format!("Concept: {}", name),
                content: Content::Plain(
                    "This is a conceptual pattern about design principles.".to_string(),
                ),
                maturity: Maturity::Draft,
                ..Default::default()
            },
            attachments: vec![],
        }
    }

    #[test]
    fn test_detect_workflow_candidates() {
        let bridge = CommanderBridge::with_defaults();
        let patterns = vec![
            make_automatable_pattern("rust-ci"),
            make_non_automatable_pattern("design-thinking"),
        ];

        let candidates = bridge.detect_workflow_candidates(&patterns);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].pattern_name, "rust-ci");
        assert!(candidates[0].confidence > 0.5);
    }

    #[test]
    fn test_detect_skips_low_maturity() {
        let bridge = CommanderBridge::with_defaults();
        let mut pattern = make_automatable_pattern("test");
        pattern.base.maturity = Maturity::Emerging;

        let candidates = bridge.detect_workflow_candidates(&[pattern]);
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_detect_skips_low_effectiveness() {
        let bridge = CommanderBridge::with_defaults();
        let mut pattern = make_automatable_pattern("test");
        pattern.base.evidence.success_signals = 1;
        pattern.base.evidence.override_signals = 10;

        let candidates = bridge.detect_workflow_candidates(&[pattern]);
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_detect_skips_no_actions() {
        let bridge = CommanderBridge::with_defaults();
        let mut pattern = make_automatable_pattern("test");
        pattern.base.content =
            Content::Plain("This is just a description without commands.".to_string());

        let candidates = bridge.detect_workflow_candidates(&[pattern]);
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_canonical_gets_confidence_boost() {
        let bridge = CommanderBridge::with_defaults();
        let mut stable = make_automatable_pattern("stable");
        let mut canonical = make_automatable_pattern("canonical");
        canonical.base.maturity = Maturity::Canonical;

        let stable_candidates = bridge.detect_workflow_candidates(&[stable.clone()]);
        let canonical_candidates = bridge.detect_workflow_candidates(&[canonical.clone()]);

        assert!(canonical_candidates[0].confidence > stable_candidates[0].confidence);
    }

    #[test]
    fn test_pattern_to_commander_yaml() {
        let bridge = CommanderBridge::with_defaults();
        let pattern = make_automatable_pattern("rust-ci");

        let yaml = bridge.pattern_to_commander_yaml(&pattern).unwrap();
        assert!(yaml.contains("cmd-rust-ci"));
        assert!(yaml.contains("cargo build"));
        assert!(yaml.contains("cargo test"));
    }

    #[test]
    fn test_suggest_workflow_automatable() {
        let bridge = CommanderBridge::with_defaults();
        let pattern = make_automatable_pattern("rust-ci");

        let preview = bridge.suggest_workflow(&pattern).unwrap();
        assert!(preview.is_some());
        let preview = preview.unwrap();
        assert_eq!(preview.workflow.name, "cmd-rust-ci");
        assert_eq!(preview.workflow.steps.len(), 3);
        assert!(preview.yaml_content.contains("cargo"));
    }

    #[test]
    fn test_suggest_workflow_non_automatable() {
        let bridge = CommanderBridge::with_defaults();
        let pattern = make_non_automatable_pattern("concept");

        let preview = bridge.suggest_workflow(&pattern).unwrap();
        assert!(preview.is_none());
    }

    #[test]
    fn test_save_workflow() {
        let tmp = TempDir::new().unwrap();
        let bridge = CommanderBridge::new(CommanderBridgeConfig {
            workflows_dir: tmp.path().to_path_buf(),
            auto_suggest: false,
        });
        let pattern = make_automatable_pattern("rust-ci");
        let workflow = bridge.build_workflow(&pattern);

        let path = bridge.save_workflow(&workflow).unwrap();
        assert!(path.exists());
        assert!(path.to_string_lossy().contains("cmd-rust-ci.yaml"));

        // Verify the saved file is valid YAML
        let content = fs::read_to_string(&path).unwrap();
        let loaded: Workflow = serde_yaml::from_str(&content).unwrap();
        assert_eq!(loaded.name, "cmd-rust-ci");
        assert_eq!(loaded.steps.len(), 3);
    }

    #[test]
    fn test_extract_steps_with_dollar_prefix() {
        let content = "$ cargo build\n$ cargo test --release\nSome description line\n$ cargo clippy";
        let pattern = make_automatable_pattern("test");
        let steps = extract_steps(content, &pattern);

        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].command.as_deref(), Some("cargo build"));
        assert_eq!(steps[0].tool.as_deref(), Some("cargo"));
        assert_eq!(steps[1].command.as_deref(), Some("cargo test --release"));
        assert_eq!(steps[2].command.as_deref(), Some("cargo clippy"));
    }

    #[test]
    fn test_extract_steps_with_action_keywords() {
        let content = "run tests first\nbuild the project\nthis is just a note\ndeploy to staging";
        let pattern = make_automatable_pattern("test");
        let steps = extract_steps(content, &pattern);

        assert_eq!(steps.len(), 3);
    }

    #[test]
    fn test_extract_steps_fallback_single_step() {
        let content = "This pattern has no commands at all, just principles.";
        let pattern = make_automatable_pattern("test");
        let steps = extract_steps(content, &pattern);

        assert_eq!(steps.len(), 1);
        assert!(steps[0].needs_approval);
        assert!(steps[0].command.is_none());
    }

    #[test]
    fn test_dual_layer_content_extraction() {
        let bridge = CommanderBridge::with_defaults();
        let mut pattern = make_automatable_pattern("dual");
        pattern.base.content = Content::DualLayer {
            technical: "$ cargo build\n$ cargo test".to_string(),
            principle: Some("Always test before deploying.".to_string()),
        };

        let preview = bridge.suggest_workflow(&pattern).unwrap().unwrap();
        assert_eq!(preview.workflow.steps.len(), 2);
    }

    #[test]
    fn test_workflow_inherits_pattern_metadata() {
        let bridge = CommanderBridge::with_defaults();
        let mut pattern = make_automatable_pattern("full-meta");
        pattern.base.applies.tools = vec!["cargo".into(), "docker".into()];
        pattern.base.tags.topics = vec!["ci".into(), "deployment".into()];

        let preview = bridge.suggest_workflow(&pattern).unwrap().unwrap();
        let wf = &preview.workflow;
        assert!(wf.trigger.contains("ci"));
        assert!(wf.trigger.contains("deployment"));
        assert!(wf.tools.contains(&"cargo".to_string()));
        assert!(wf.tools.contains(&"docker".to_string()));
    }

    #[test]
    fn test_workflow_exists_helper() {
        let tmp = TempDir::new().unwrap();
        assert!(!workflow_exists(tmp.path(), "nonexistent"));

        // Create a file
        fs::write(tmp.path().join("cmd-test.yaml"), "test").unwrap();
        assert!(workflow_exists(tmp.path(), "test"));
    }

    #[test]
    fn test_config_default() {
        let config = CommanderBridgeConfig::default();
        assert!(config.auto_suggest);
        assert!(config.workflows_dir.to_string_lossy().contains(".mur"));
        assert!(config
            .workflows_dir
            .to_string_lossy()
            .contains("workflows"));
    }

    #[test]
    fn test_infer_tool_known() {
        let pattern = make_automatable_pattern("test");
        assert_eq!(
            infer_tool("cargo build", &pattern),
            Some("cargo".to_string())
        );
        assert_eq!(
            infer_tool("docker compose up", &pattern),
            Some("docker".to_string())
        );
        assert_eq!(
            infer_tool("npm install", &pattern),
            Some("npm".to_string())
        );
        assert_eq!(
            infer_tool("git push origin main", &pattern),
            Some("git".to_string())
        );
    }

    #[test]
    fn test_collect_tools_deduplicates() {
        let mut pattern = make_automatable_pattern("test");
        pattern.base.applies.tools = vec!["cargo".into()];

        let steps = vec![
            Step {
                order: 1,
                description: "build".into(),
                command: Some("cargo build".into()),
                tool: Some("cargo".into()),
                needs_approval: false,
                on_failure: FailureAction::Abort,
            },
            Step {
                order: 2,
                description: "test".into(),
                command: Some("npm test".into()),
                tool: Some("npm".into()),
                needs_approval: false,
                on_failure: FailureAction::Abort,
            },
        ];

        let tools = collect_tools(&pattern, &steps);
        assert_eq!(tools, vec!["cargo", "npm"]);
    }
}
