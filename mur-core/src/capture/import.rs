//! Import rules from AI tool config files and convert them into MUR patterns.
//!
//! Supported files:
//! - Markdown: CLAUDE.md, AGENTS.md, .clinerules
//! - Plain text / markdown: .cursorrules, .windsurfrules
//! - Markdown: .github/copilot-instructions.md

use anyhow::Result;
use chrono::Utc;
use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// A candidate pattern extracted from an AI tool config file.
#[derive(Debug, Clone)]
pub struct ImportCandidate {
    pub name: String,
    pub description: String,
    pub content: String,
    pub kind: PatternKind,
    pub source_file: String,
    pub source_tag: String,
}

/// Well-known AI tool config files to scan for.
const KNOWN_FILES: &[&str] = &[
    ".cursorrules",
    ".windsurfrules",
    ".clinerules",
    "CLAUDE.md",
    "AGENTS.md",
    ".github/copilot-instructions.md",
];

/// Detect importable AI tool config files in a directory.
pub fn detect_files(dir: &Path) -> Vec<PathBuf> {
    KNOWN_FILES
        .iter()
        .map(|f| dir.join(f))
        .filter(|p| p.exists())
        .collect()
}

/// Extract import candidates from a single file.
pub fn extract_from_file(path: &Path) -> Result<Vec<ImportCandidate>> {
    let content = std::fs::read_to_string(path)?;
    let filename = path.file_name().unwrap_or_default().to_string_lossy();
    let source_tag = source_tag_from_filename(&filename);

    // Strip MUR sync markers
    let content = strip_mur_markers(&content);

    let has_headers = content.lines().any(|l| l.starts_with('#'));

    if has_headers {
        extract_markdown_sections(&content, &filename, &source_tag)
    } else {
        extract_paragraphs(&content, &filename, &source_tag)
    }
}

/// Convert import candidates into Pattern objects, skipping duplicates.
pub fn candidates_to_patterns(
    candidates: Vec<ImportCandidate>,
    existing_names: &HashSet<String>,
) -> Vec<Pattern> {
    let mut seen = HashSet::new();
    let mut patterns = Vec::new();

    for c in candidates {
        if existing_names.contains(&c.name) || !seen.insert(c.name.clone()) {
            continue;
        }

        let technical = truncate_content(&c.content, Content::MAX_LAYER_CHARS);
        let now = Utc::now();

        let pattern = Pattern {
            base: KnowledgeBase {
                schema: SCHEMA_VERSION,
                name: c.name,
                description: c.description,
                content: Content::DualLayer {
                    technical,
                    principle: None,
                },
                tier: Tier::Project,
                importance: 0.5,
                confidence: 0.7,
                tags: Tags {
                    languages: vec![],
                    topics: vec!["imported".to_string(), c.source_tag.clone()],
                    extra: Default::default(),
                },
                applies: Applies::default(),
                evidence: Evidence {
                    first_seen: Some(now),
                    ..Default::default()
                },
                links: Links::default(),
                lifecycle: Lifecycle::default(),
                created_at: now,
                updated_at: now,
                maturity: mur_common::knowledge::Maturity::Draft,
                decay: Default::default(),
            },
            kind: Some(c.kind),
            origin: Some(Origin {
                source: "import".to_string(),
                trigger: OriginTrigger::UserExplicit,
                user: None,
                platform: Some(c.source_file),
                confidence: 0.7,
            }),
            attachments: vec![],
        };
        patterns.push(pattern);
    }

    patterns
}

// ─── Internal helpers ────────────────────────────────────────────

/// Split markdown content by ## headers (fall back to # if no ## found).
fn extract_markdown_sections(
    content: &str,
    filename: &str,
    source_tag: &str,
) -> Result<Vec<ImportCandidate>> {
    let use_h2 = content.lines().any(|l| l.starts_with("## "));
    let prefix = if use_h2 { "## " } else { "# " };

    let mut candidates = Vec::new();
    let mut current_header: Option<String> = None;
    let mut current_body = String::new();

    for line in content.lines() {
        if line.starts_with(prefix) {
            // Flush previous section
            if let Some(header) = current_header.take()
                && let Some(c) = make_candidate(&header, &current_body, filename, source_tag)
            {
                candidates.push(c);
            }
            current_header = Some(line.trim_start_matches('#').trim().to_string());
            current_body.clear();
        } else if current_header.is_some() {
            current_body.push_str(line);
            current_body.push('\n');
        }
    }

    // Flush last section
    if let Some(header) = current_header
        && let Some(c) = make_candidate(&header, &current_body, filename, source_tag)
    {
        candidates.push(c);
    }

    Ok(candidates)
}

/// Split plain text by double newlines into paragraphs.
fn extract_paragraphs(
    content: &str,
    filename: &str,
    source_tag: &str,
) -> Result<Vec<ImportCandidate>> {
    let mut candidates = Vec::new();

    for (i, paragraph) in content.split("\n\n").enumerate() {
        let text = paragraph.trim();
        if text.len() <= 50 {
            continue;
        }
        if is_purely_commands(text) {
            continue;
        }

        let name = name_from_text(text, i);
        let description = first_sentence(text);
        let kind = infer_kind(text);

        candidates.push(ImportCandidate {
            name,
            description,
            content: text.to_string(),
            kind,
            source_file: filename.to_string(),
            source_tag: source_tag.to_string(),
        });
    }

    Ok(candidates)
}

/// Try to create a candidate from a markdown section header + body.
/// Returns None if the section is too short or purely structural.
fn make_candidate(
    header: &str,
    body: &str,
    filename: &str,
    source_tag: &str,
) -> Option<ImportCandidate> {
    let body = body.trim();

    // Skip sections with too little meaningful content
    if body.len() <= 50 {
        return None;
    }

    // Skip structural sections
    if is_structural_header(header) {
        return None;
    }

    // Skip sections that are purely shell commands
    if is_purely_commands(body) {
        return None;
    }

    let name = slugify(header);
    let description = first_sentence(body);
    let kind = infer_kind(body);

    Some(ImportCandidate {
        name,
        description,
        content: body.to_string(),
        kind,
        source_file: filename.to_string(),
        source_tag: source_tag.to_string(),
    })
}

/// Infer PatternKind from content text.
pub fn infer_kind(text: &str) -> PatternKind {
    let lower = text.to_lowercase();

    // Check for preference indicators
    let preference_words = [
        "prefer ",
        "always ",
        "never ",
        "use x over",
        "use ",
        " over ",
        "instead of",
        "avoid ",
        "don't use",
        "do not use",
    ];
    if preference_words.iter().any(|w| lower.contains(w)) {
        return PatternKind::Preference;
    }

    // Check for procedural indicators (numbered steps)
    let has_numbered_steps = text
        .lines()
        .filter(|l| {
            let trimmed = l.trim();
            trimmed.starts_with("1.") || trimmed.starts_with("2.") || trimmed.starts_with("3.")
        })
        .count()
        >= 2;
    if has_numbered_steps {
        return PatternKind::Procedure;
    }

    // Check for behavioral rules
    let behavioral_words = ["must ", "shall ", "should ", "required"];
    if behavioral_words.iter().any(|w| lower.contains(w)) {
        return PatternKind::Behavioral;
    }

    PatternKind::Technical
}

/// Convert a header string to a kebab-case slug.
pub fn slugify(header: &str) -> String {
    let slug: String = header
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();

    // Collapse consecutive dashes and trim
    let mut result = String::new();
    let mut prev_dash = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_dash && !result.is_empty() {
                result.push('-');
            }
            prev_dash = true;
        } else {
            result.push(c);
            prev_dash = false;
        }
    }
    result.trim_matches('-').to_string()
}

/// Generate a pattern name from text content (for non-markdown paragraphs).
fn name_from_text(text: &str, index: usize) -> String {
    let words: Vec<&str> = text
        .split_whitespace()
        .filter(|w| w.len() > 2)
        .take(4)
        .collect();

    if words.is_empty() {
        return format!("rule-{}", index + 1);
    }

    slugify(&words.join(" "))
}

/// Extract the first sentence from text for use as description.
fn first_sentence(text: &str) -> String {
    let first_line = text.lines().next().unwrap_or(text);
    let trimmed = first_line
        .trim()
        .trim_start_matches("- ")
        .trim_start_matches("* ");

    // Truncate at sentence boundary or 120 chars
    if let Some(pos) = trimmed.find(". ") {
        trimmed[..=pos].to_string()
    } else if trimmed.len() > 120 {
        format!("{}...", &trimmed[..117])
    } else {
        trimmed.to_string()
    }
}

/// Check if a section header is purely structural (TOC, Commands, etc.).
fn is_structural_header(header: &str) -> bool {
    let lower = header.to_lowercase();
    let structural = [
        "table of contents",
        "toc",
        "index",
        "changelog",
        "license",
        "credits",
        "acknowledgments",
    ];
    structural.iter().any(|s| lower.contains(s))
}

/// Check if text is purely shell commands with no explanation.
fn is_purely_commands(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return true;
    }

    let command_lines = lines
        .iter()
        .filter(|l| {
            let trimmed = l.trim();
            trimmed.starts_with('$')
                || trimmed.starts_with("```")
                || trimmed.starts_with('#')
                || trimmed.starts_with("//")
                || (trimmed.contains("--") && !trimmed.contains(' '))
        })
        .count();

    // If >80% of lines look like commands/code, skip
    command_lines * 100 / lines.len() > 80
}

/// Strip MUR sync markers and their content.
fn strip_mur_markers(content: &str) -> String {
    let mut result = String::new();
    let mut in_mur_block = false;

    for line in content.lines() {
        if line.contains("<!-- MUR:START -->") {
            in_mur_block = true;
            continue;
        }
        if line.contains("<!-- MUR:END -->") {
            in_mur_block = false;
            continue;
        }
        if !in_mur_block {
            result.push_str(line);
            result.push('\n');
        }
    }

    result
}

/// Map filename to a source tag for the pattern.
fn source_tag_from_filename(filename: &str) -> String {
    match filename {
        ".cursorrules" => "cursorrules".to_string(),
        ".windsurfrules" => "windsurfrules".to_string(),
        ".clinerules" => "clinerules".to_string(),
        "CLAUDE.md" => "claude-md".to_string(),
        "AGENTS.md" => "agents-md".to_string(),
        "copilot-instructions.md" => "copilot-instructions".to_string(),
        _ => filename.to_lowercase().replace('.', "-"),
    }
}

/// Truncate content to max chars on a word boundary.
fn truncate_content(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    // Find last space before max
    let truncated = &text[..max];
    if let Some(pos) = truncated.rfind(' ') {
        format!("{}...", &text[..pos])
    } else {
        format!("{}...", truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slugify() {
        assert_eq!(slugify("Error Handling"), "error-handling");
        assert_eq!(
            slugify("Use TypeScript  Strict Mode"),
            "use-typescript-strict-mode"
        );
        assert_eq!(slugify("  Spaces & Symbols! "), "spaces-symbols");
        assert_eq!(slugify("API Design"), "api-design");
    }

    #[test]
    fn test_infer_kind_preference() {
        assert_eq!(
            infer_kind("Always use async/await instead of callbacks"),
            PatternKind::Preference
        );
        assert_eq!(
            infer_kind("Prefer composition over inheritance"),
            PatternKind::Preference
        );
        assert_eq!(
            infer_kind("Never use var, use let or const"),
            PatternKind::Preference
        );
        assert_eq!(
            infer_kind("Avoid using any type in TypeScript"),
            PatternKind::Preference
        );
    }

    #[test]
    fn test_infer_kind_procedure() {
        let text =
            "To deploy:\n1. Build the project\n2. Run tests\n3. Push to main\n4. Deploy to staging";
        assert_eq!(infer_kind(text), PatternKind::Procedure);
    }

    #[test]
    fn test_infer_kind_technical() {
        assert_eq!(
            infer_kind("The architecture follows a hexagonal pattern with ports and adapters"),
            PatternKind::Technical
        );
    }

    #[test]
    fn test_infer_kind_behavioral() {
        assert_eq!(
            infer_kind("You must run linting before committing code to the repo"),
            PatternKind::Behavioral
        );
    }

    #[test]
    fn test_markdown_splitting() {
        let content = "# Project\n\nOverview paragraph that is short.\n\n## Error Handling\n\nAlways wrap async operations in try-catch blocks. Use custom error types for domain errors. This ensures consistent error handling across the codebase.\n\n## Testing\n\nWrite tests for all public functions. Use integration tests for API endpoints. Coverage should be above 80 percent minimum.\n";

        let candidates = extract_markdown_sections(content, "CLAUDE.md", "claude-md").unwrap();
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].name, "error-handling");
        assert_eq!(candidates[1].name, "testing");
    }

    #[test]
    fn test_paragraph_splitting() {
        let content = "Short.\n\nAlways use TypeScript strict mode for better type safety. This catches common errors at compile time and improves code quality.\n\nAnother short.\n\nFollow the repository naming convention: kebab-case for directories, PascalCase for components. This keeps the project structure consistent and easy to navigate.\n";

        let candidates = extract_paragraphs(content, ".cursorrules", "cursorrules").unwrap();
        assert_eq!(candidates.len(), 2);
        assert!(candidates[0].content.contains("TypeScript strict mode"));
        assert!(candidates[1].content.contains("kebab-case"));
    }

    #[test]
    fn test_skip_short_sections() {
        let content = "## Commands\n\nJust run it.\n\n## Architecture\n\nThe system uses a layered architecture with clear separation of concerns. The data layer handles persistence, the service layer contains business logic, and the API layer manages HTTP concerns.\n";

        let candidates = extract_markdown_sections(content, "CLAUDE.md", "claude-md").unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].name, "architecture");
    }

    #[test]
    fn test_skip_purely_commands() {
        let content = "## Build\n\n```bash\ncargo build\ncargo test\ncargo clippy\n```\n\n## Style Guide\n\nUse 2-space indentation for TypeScript. Prefer named exports over default exports. Keep functions under 30 lines for readability.\n";

        let candidates = extract_markdown_sections(content, "CLAUDE.md", "claude-md").unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].name, "style-guide");
    }

    #[test]
    fn test_dedup_existing_patterns() {
        let candidates = vec![
            ImportCandidate {
                name: "error-handling".to_string(),
                description: "Handle errors".to_string(),
                content: "Always use try-catch for async operations in the codebase".to_string(),
                kind: PatternKind::Preference,
                source_file: "CLAUDE.md".to_string(),
                source_tag: "claude-md".to_string(),
            },
            ImportCandidate {
                name: "new-rule".to_string(),
                description: "New rule".to_string(),
                content: "Some new rule content that should be imported as a pattern".to_string(),
                kind: PatternKind::Technical,
                source_file: "CLAUDE.md".to_string(),
                source_tag: "claude-md".to_string(),
            },
        ];

        let existing: HashSet<String> = ["error-handling".to_string()].into();
        let patterns = candidates_to_patterns(candidates, &existing);
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0].name, "new-rule");
    }

    #[test]
    fn test_strip_mur_markers() {
        let content =
            "Some text\n<!-- MUR:START -->\ninjected stuff\n<!-- MUR:END -->\nMore text\n";
        let stripped = strip_mur_markers(content);
        assert!(stripped.contains("Some text"));
        assert!(stripped.contains("More text"));
        assert!(!stripped.contains("injected stuff"));
    }

    #[test]
    fn test_name_from_text() {
        let text = "Always use strict TypeScript mode for safety";
        assert_eq!(name_from_text(text, 0), "always-use-strict-typescript");
    }

    #[test]
    fn test_source_tag() {
        assert_eq!(source_tag_from_filename(".cursorrules"), "cursorrules");
        assert_eq!(source_tag_from_filename("CLAUDE.md"), "claude-md");
        assert_eq!(
            source_tag_from_filename("copilot-instructions.md"),
            "copilot-instructions"
        );
    }
}
