//! Schema validation enforced after parsing.

use super::manifest::SkillManifest;
use super::mcp;
use super::types::{Category, ContentMode, TriggerKind};
use std::fmt;

#[derive(Debug, PartialEq, Eq)]
pub enum ValidationError {
    InvalidName(String),
    InvalidVersion(String),
    InvalidPublisher(String),
    NoContentMode,
    MultipleContentModes,
    ContentModeMismatch {
        category: Category,
        mode: ContentMode,
    },
    TriggerMissingPattern(TriggerKind),
    EmptyAbstract,
    /// Invalid mcp_requirements entry. Fields: index, message.
    McpRequirements(usize, String),
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use ValidationError::*;
        match self {
            InvalidName(n) => write!(f, "invalid skill name '{n}' (must match [a-z0-9-]{{1,64}})"),
            InvalidVersion(v) => write!(f, "invalid version '{v}' (expected MAJOR.MINOR.PATCH)"),
            InvalidPublisher(p) => write!(
                f,
                "invalid publisher '{p}' (expected 'human:<n>' or 'agent:<id>')"
            ),
            NoContentMode => write!(
                f,
                "content must populate exactly one of: context / procedure / command / note"
            ),
            MultipleContentModes => write!(
                f,
                "content must populate only one of: context / procedure / command / note"
            ),
            ContentModeMismatch { category, mode } => {
                write!(
                    f,
                    "category {category:?} does not match content mode {mode:?}"
                )
            }
            TriggerMissingPattern(k) => write!(f, "trigger '{k:?}' requires a `pattern` field"),
            EmptyAbstract => write!(f, "content.abstract must not be empty"),
            McpRequirements(idx, msg) => {
                write!(f, "mcp_requirements[{idx}]: {msg}")
            }
        }
    }
}

impl std::error::Error for ValidationError {}

pub fn validate(m: &SkillManifest) -> Result<(), ValidationError> {
    validate_name(&m.name)?;
    validate_version(&m.version)?;
    validate_publisher(&m.publisher)?;

    if m.content.r#abstract.trim().is_empty() {
        return Err(ValidationError::EmptyAbstract);
    }

    let mode = m.content.mode().ok_or_else(|| {
        let populated = [
            m.content.context.is_some(),
            m.content.procedure.is_some(),
            m.content.command.is_some(),
            m.content.note.is_some(),
        ]
        .iter()
        .filter(|b| **b)
        .count();
        if populated > 1 {
            ValidationError::MultipleContentModes
        } else {
            ValidationError::NoContentMode
        }
    })?;

    if !mode_matches_category(m.category, mode) {
        return Err(ValidationError::ContentModeMismatch {
            category: m.category,
            mode,
        });
    }

    for t in &m.triggers {
        if matches!(t.kind, TriggerKind::Command | TriggerKind::Keyword) && t.pattern.is_none() {
            return Err(ValidationError::TriggerMissingPattern(t.kind));
        }
    }

    if let Err((idx, msg)) = mcp::validate_requirements(&m.mcp_requirements) {
        return Err(ValidationError::McpRequirements(idx, msg));
    }

    // Validate intent + tool_hint on procedure steps (v2.2).
    if let Some(proc) = &m.content.procedure {
        for (idx, step) in proc.steps.iter().enumerate() {
            if let Some(hint) = &step.tool_hint
                && hint.is_empty()
            {
                return Err(ValidationError::McpRequirements(
                    idx,
                    "tool_hint must not be empty when present".into(),
                ));
            }
            if let Some(intent) = &step.intent
                && intent.is_empty()
            {
                return Err(ValidationError::McpRequirements(
                    idx,
                    "intent must not be empty when present".into(),
                ));
            }
        }
    }

    Ok(())
}

fn validate_name(name: &str) -> Result<(), ValidationError> {
    if name.is_empty() || name.len() > 64 {
        return Err(ValidationError::InvalidName(name.into()));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(ValidationError::InvalidName(name.into()));
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err(ValidationError::InvalidName(name.into()));
    }
    Ok(())
}

fn validate_version(v: &str) -> Result<(), ValidationError> {
    let parts: Vec<&str> = v.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()))
    {
        return Err(ValidationError::InvalidVersion(v.into()));
    }
    Ok(())
}

fn validate_publisher(p: &str) -> Result<(), ValidationError> {
    let (kind, rest) = p
        .split_once(':')
        .ok_or_else(|| ValidationError::InvalidPublisher(p.into()))?;
    if rest.is_empty() {
        return Err(ValidationError::InvalidPublisher(p.into()));
    }
    match kind {
        "human" | "agent" => Ok(()),
        _ => Err(ValidationError::InvalidPublisher(p.into())),
    }
}

fn mode_matches_category(cat: Category, mode: ContentMode) -> bool {
    matches!(
        (cat, mode),
        (Category::Workflow, ContentMode::Workflow)
            // Dev-discipline builtins are workflow-category skills whose body
            // is method prose (`context`), not executable steps.
            | (Category::Workflow, ContentMode::Context)
            | (Category::Command, ContentMode::Command)
            | (Category::Context, ContentMode::Context)
            | (Category::Meta, ContentMode::Context)
            | (Category::Note, ContentMode::Note)
            // Media skills carry a `context` body (the when/why prose the agent
            // reads); there is no dedicated ContentMode::Media.
            | (Category::Media, ContentMode::Context)
    )
}

#[cfg(test)]
mod tests {
    use super::super::parser::parse_canonical;
    use super::*;

    const VALID: &str = r#"
name: demo
version: 1.0.0
publisher: human:test
description: d
category: context
content:
  abstract: hi
  context: body
"#;

    #[test]
    fn valid_manifest_passes() {
        let m = parse_canonical(VALID).unwrap();
        validate(&m).unwrap();
    }

    /// Workflow-category skills may carry a `context` body (dev-discipline
    /// builtin convention: method prose, not executable steps).
    #[test]
    fn workflow_category_accepts_context_mode() {
        let m = parse_canonical(&VALID.replace("category: context", "category: workflow")).unwrap();
        validate(&m).unwrap();
    }

    #[test]
    fn rejects_uppercase_name() {
        let mut m = parse_canonical(VALID).unwrap();
        m.name = "Demo".into();
        assert!(matches!(validate(&m), Err(ValidationError::InvalidName(_))));
    }

    #[test]
    fn rejects_bad_version() {
        let mut m = parse_canonical(VALID).unwrap();
        m.version = "1.0".into();
        assert!(matches!(
            validate(&m),
            Err(ValidationError::InvalidVersion(_))
        ));
    }

    #[test]
    fn rejects_bad_publisher() {
        let mut m = parse_canonical(VALID).unwrap();
        m.publisher = "anon".into();
        assert!(matches!(
            validate(&m),
            Err(ValidationError::InvalidPublisher(_))
        ));
    }

    #[test]
    fn rejects_category_mode_mismatch() {
        let yaml = r#"
name: demo
version: 1.0.0
publisher: human:test
description: d
category: note
content:
  abstract: hi
  context: oops
"#;
        let m = parse_canonical(yaml).unwrap();
        assert!(matches!(
            validate(&m),
            Err(ValidationError::ContentModeMismatch { .. })
        ));
    }

    #[test]
    fn media_category_with_context_validates() {
        // The shipped media skills (video-analyze/scene-explain/vlc-control/
        // watch-together) declare `category: media` with a `context` body;
        // validation must accept them so `mur agent skill add` doesn't reject them.
        let yaml = r#"
name: video-analyze
version: 1.0.0
publisher: human:test
description: analyze a video
category: media
content:
  abstract: hi
  context: when and why to use video_analyze
"#;
        let m = parse_canonical(yaml).unwrap();
        validate(&m).expect("media + context should validate");
    }

    // ── M6a: mcp_requirements ──

    #[test]
    fn valid_mcp_requirements_passes() {
        let yaml = r#"
name: demo
version: 1.0.0
publisher: human:test
description: d
category: workflow
content:
  abstract: hi
  procedure:
    steps:
      - description: test
mcp_requirements:
  - tool_pattern: "browser.*"
    capability: network_http
"#;
        let m = parse_canonical(yaml).unwrap();
        validate(&m).unwrap();
    }

    #[test]
    fn empty_mcp_requirements_passes() {
        let yaml = r#"
name: demo
version: 1.0.0
publisher: human:test
description: d
category: context
content:
  abstract: hi
  context: body
"#;
        let m = parse_canonical(yaml).unwrap();
        validate(&m).unwrap();
    }

    #[test]
    fn rejects_duplicate_mcp_requirements() {
        let yaml = r#"
name: demo
version: 1.0.0
publisher: human:test
description: d
category: workflow
content:
  abstract: hi
  procedure:
    steps:
      - description: test
mcp_requirements:
  - tool_pattern: "fs.*"
    capability: read_file
  - tool_pattern: "fs.*"
    capability: read_file
"#;
        let m = parse_canonical(yaml).unwrap();
        assert!(matches!(
            validate(&m),
            Err(ValidationError::McpRequirements(1, _))
        ));
    }

    #[test]
    fn rejects_empty_mcp_pattern() {
        let yaml = r#"
name: demo
version: 1.0.0
publisher: human:test
description: d
category: workflow
content:
  abstract: hi
  procedure:
    steps:
      - description: test
mcp_requirements:
  - tool_pattern: ""
    capability: read_file
"#;
        let m = parse_canonical(yaml).unwrap();
        assert!(matches!(
            validate(&m),
            Err(ValidationError::McpRequirements(0, _))
        ));
    }

    #[test]
    fn command_trigger_requires_pattern() {
        let yaml = r#"
name: demo
version: 1.0.0
publisher: human:test
description: d
category: context
content:
  abstract: hi
  context: body
triggers:
  - type: command
"#;
        let m = parse_canonical(yaml).unwrap();
        assert!(matches!(
            validate(&m),
            Err(ValidationError::TriggerMissingPattern(TriggerKind::Command))
        ));
    }

    #[test]
    fn valid_note_manifest_passes() {
        let yaml = "name: rust-notes\nversion: 1.0.0\npublisher: human:test\n\
                    category: note\ndescription: d\n\
                    content:\n  abstract: a\n  note: |\n    # body\n";
        let m = parse_canonical(yaml).unwrap();
        assert!(validate(&m).is_ok());
    }

    #[test]
    fn note_category_with_context_mode_is_mismatch() {
        let yaml = "name: rust-notes\nversion: 1.0.0\npublisher: human:test\n\
                    category: note\ndescription: d\n\
                    content:\n  abstract: a\n  context: c\n";
        let m = parse_canonical(yaml).unwrap();
        assert!(matches!(
            validate(&m),
            Err(ValidationError::ContentModeMismatch { .. })
        ));
    }

    #[test]
    fn note_plus_command_is_multiple_modes() {
        let yaml = "name: rust-notes\nversion: 1.0.0\npublisher: human:test\n\
                    category: note\ndescription: d\n\
                    content:\n  abstract: a\n  note: x\n  command: y\n";
        let m = parse_canonical(yaml).unwrap();
        assert!(matches!(
            validate(&m),
            Err(ValidationError::MultipleContentModes)
        ));
    }
}
