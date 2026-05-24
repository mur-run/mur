//! Dual-format parser. Canonical YAML is the source of truth; markdown
//! frontmatter is the human-authoring surface that round-trips via
//! `canonical_from_markdown()` / `markdown_from_canonical()`.

use super::manifest::SkillManifest;
use std::fmt;

#[derive(Debug)]
pub enum ParseError {
    Yaml(serde_yaml_ng::Error),
    MissingFrontmatter,
    MalformedFrontmatter(String),
    LegacyMarkdown(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Yaml(e) => write!(f, "yaml parse: {e}"),
            ParseError::MissingFrontmatter => write!(f, "missing `---` frontmatter delimiters"),
            ParseError::MalformedFrontmatter(s) => write!(f, "malformed frontmatter: {s}"),
            ParseError::LegacyMarkdown(s) => write!(f, "legacy markdown: {s}"),
        }
    }
}

impl std::error::Error for ParseError {}

impl From<serde_yaml_ng::Error> for ParseError {
    fn from(e: serde_yaml_ng::Error) -> Self {
        ParseError::Yaml(e)
    }
}

/// Parse canonical `skill.yaml`.
pub fn parse_canonical(yaml: &str) -> Result<SkillManifest, ParseError> {
    let m: SkillManifest = serde_yaml_ng::from_str(yaml)?;
    Ok(m)
}

/// Serialise a `SkillManifest` to canonical YAML. Deterministic field order
/// matches the struct definition.
pub fn serialize_canonical(m: &SkillManifest) -> Result<String, ParseError> {
    Ok(serde_yaml_ng::to_string(m)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
name: demo-skill
version: 0.1.0
publisher: human:test
description: Demo
category: context
content:
  abstract: hello
  context: |
    body
"#;

    #[test]
    fn parses_canonical_yaml() {
        let m = parse_canonical(SAMPLE).unwrap();
        assert_eq!(m.name, "demo-skill");
        assert_eq!(m.content.context.as_deref(), Some("body\n"));
    }

    #[test]
    fn serialize_then_reparse_is_identity() {
        let m = parse_canonical(SAMPLE).unwrap();
        let yaml = serialize_canonical(&m).unwrap();
        let m2 = parse_canonical(&yaml).unwrap();
        assert_eq!(m.name, m2.name);
        assert_eq!(m.content.context, m2.content.context);
    }

    #[test]
    fn rejects_non_yaml_input() {
        let r = parse_canonical("this is not yaml ::: {{");
        assert!(r.is_err());
    }
}
