//! Schema validation enforced after parsing.

use super::manifest::SkillManifest;
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
                "content must populate exactly one of: context / procedure / command"
            ),
            MultipleContentModes => write!(
                f,
                "content must populate only one of: context / procedure / command"
            ),
            ContentModeMismatch { category, mode } => {
                write!(
                    f,
                    "category {category:?} does not match content mode {mode:?}"
                )
            }
            TriggerMissingPattern(k) => write!(f, "trigger '{k:?}' requires a `pattern` field"),
            EmptyAbstract => write!(f, "content.abstract must not be empty"),
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
            | (Category::Command, ContentMode::Command)
            | (Category::Context, ContentMode::Context)
            | (Category::Meta, ContentMode::Context)
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
category: workflow
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
}
