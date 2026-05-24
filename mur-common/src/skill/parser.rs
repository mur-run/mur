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

/// Parse markdown-frontmatter skill source. Frontmatter (between two `---`
/// fences) is YAML; the body becomes `content.abstract` plus — if it has a
/// `## Steps` heading — a synthesised `content.procedure`, or otherwise a
/// `content.context`. This is the human-authoring surface; canonical YAML
/// remains source of truth on disk.
pub fn parse_markdown(input: &str) -> Result<SkillManifest, ParseError> {
    let (frontmatter, body) = split_frontmatter(input)?;
    let mut value: serde_yaml_ng::Value = serde_yaml_ng::from_str(frontmatter)?;
    inject_content_from_body(&mut value, body)?;
    let m: SkillManifest = serde_yaml_ng::from_value(value)?;
    Ok(m)
}

fn split_frontmatter(input: &str) -> Result<(&str, &str), ParseError> {
    let trimmed = input.trim_start_matches('\u{feff}');
    let trimmed = trimmed.strip_prefix("---").ok_or(ParseError::MissingFrontmatter)?;
    let trimmed = trimmed.strip_prefix('\n').unwrap_or(trimmed);
    let end = trimmed
        .find("\n---")
        .ok_or_else(|| ParseError::MalformedFrontmatter("missing closing `---`".into()))?;
    let frontmatter = &trimmed[..end];
    let after = &trimmed[end + 4..];
    let body = after.strip_prefix('\n').unwrap_or(after);
    Ok((frontmatter, body))
}

fn inject_content_from_body(value: &mut serde_yaml_ng::Value, body: &str) -> Result<(), ParseError> {
    use serde_yaml_ng::Value;

    if let Some(map) = value.as_mapping_mut() {
        if map.contains_key(Value::String("content".into())) {
            return Ok(()); // frontmatter already supplied content
        }
        let abstract_text = body.lines().take(3).collect::<Vec<_>>().join("\n").trim().to_string();
        let mut content = serde_yaml_ng::Mapping::new();
        content.insert(Value::String("abstract".into()), Value::String(abstract_text));

        if body.contains("## Steps") {
            let proc = build_procedure_from_steps(body);
            content.insert(Value::String("procedure".into()), proc);
        } else {
            content.insert(Value::String("context".into()), Value::String(body.trim().to_string()));
        }
        map.insert(Value::String("content".into()), Value::Mapping(content));
    } else {
        return Err(ParseError::MalformedFrontmatter("frontmatter is not a mapping".into()));
    }
    Ok(())
}

fn build_procedure_from_steps(body: &str) -> serde_yaml_ng::Value {
    use serde_yaml_ng::{Mapping, Value};
    let mut steps = Vec::new();
    let mut in_steps = false;
    for line in body.lines() {
        if line.trim_start().starts_with("## Steps") {
            in_steps = true;
            continue;
        }
        if in_steps && line.starts_with("## ") {
            break;
        }
        if in_steps {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("- ").or_else(|| {
                trimmed.find(". ").and_then(|i| {
                    let (n, r) = trimmed.split_at(i);
                    n.chars().all(|c| c.is_ascii_digit()).then(|| &r[2..])
                })
            }) {
                let mut step = Mapping::new();
                step.insert(Value::String("description".into()), Value::String(rest.to_string()));
                steps.push(Value::Mapping(step));
            }
        }
    }
    let mut procedure = Mapping::new();
    procedure.insert(Value::String("steps".into()), Value::Sequence(steps));
    Value::Mapping(procedure)
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

    #[test]
    fn parses_markdown_frontmatter_to_context_mode() {
        let md = r#"---
name: simple-md
version: 1.0.0
publisher: human:test
description: A markdown skill
category: context
---

# simple-md

Some context content here.
"#;
        let m = parse_markdown(md).unwrap();
        assert_eq!(m.name, "simple-md");
        assert!(m.content.context.is_some());
        assert!(m.content.procedure.is_none());
    }

    #[test]
    fn parses_markdown_with_steps_to_workflow_mode() {
        let md = r#"---
name: with-steps
version: 1.0.0
publisher: human:test
description: A workflow
category: workflow
---

# with-steps

Does a thing.

## Steps
1. Navigate somewhere
2. Click the button
- Final extraction step
"#;
        let m = parse_markdown(md).unwrap();
        let proc = m.content.procedure.expect("procedure populated");
        assert_eq!(proc.steps.len(), 3);
        assert_eq!(proc.steps[0].description, "Navigate somewhere");
    }

    #[test]
    fn markdown_without_frontmatter_fails() {
        let md = "# just a heading\n";
        assert!(matches!(parse_markdown(md), Err(ParseError::MissingFrontmatter)));
    }
}
