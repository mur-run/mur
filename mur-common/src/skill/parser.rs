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
    // Non-MUR skills (Claude Code plugins etc.) carry only `name` +
    // `description`; default the MUR-specific fields so external SKILL.md
    // files import seamlessly. `name`/`description` stay required.
    fill_manifest_defaults(&mut value)?;
    inject_content_from_body(&mut value, body)?;
    let m: SkillManifest = serde_yaml_ng::from_value(value)?;
    Ok(m)
}

/// Default the MUR-specific manifest fields that external (Claude-style)
/// SKILL.md frontmatter omits.
fn fill_manifest_defaults(value: &mut serde_yaml_ng::Value) -> Result<(), ParseError> {
    use serde_yaml_ng::Value;
    let map = value
        .as_mapping_mut()
        .ok_or_else(|| ParseError::MalformedFrontmatter("frontmatter is not a mapping".into()))?;
    let key = |k: &str| Value::String(k.into());
    map.entry(key("version"))
        .or_insert(Value::String("0.0.0".into()));
    map.entry(key("publisher"))
        .or_insert(Value::String("human:import".into()));
    map.entry(key("category"))
        .or_insert(Value::String("context".into()));
    Ok(())
}

fn split_frontmatter(input: &str) -> Result<(&str, &str), ParseError> {
    let trimmed = input.trim_start_matches('\u{feff}');
    let trimmed = trimmed
        .strip_prefix("---")
        .ok_or(ParseError::MissingFrontmatter)?;
    let trimmed = trimmed.strip_prefix('\n').unwrap_or(trimmed);
    let end = trimmed
        .find("\n---")
        .ok_or_else(|| ParseError::MalformedFrontmatter("missing closing `---`".into()))?;
    let frontmatter = &trimmed[..end];
    let after = &trimmed[end + 4..];
    let body = after.strip_prefix('\n').unwrap_or(after);
    Ok((frontmatter, body))
}

fn inject_content_from_body(
    value: &mut serde_yaml_ng::Value,
    body: &str,
) -> Result<(), ParseError> {
    use serde_yaml_ng::Value;

    if let Some(map) = value.as_mapping_mut() {
        if map.contains_key(Value::String("content".into())) {
            return Ok(()); // frontmatter already supplied content
        }
        let mut content = serde_yaml_ng::Mapping::new();

        if body.contains("## Steps") {
            // Workflow mode: abstract is the prose before `## Steps`, with the
            // optional leading `# Title` H1 stripped.
            let abstract_text = strip_leading_h1(body)
                .split("## Steps")
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            content.insert(
                Value::String("abstract".into()),
                Value::String(abstract_text),
            );
            let proc = build_procedure_from_steps(body);
            content.insert(Value::String("procedure".into()), proc);
        } else {
            // Context mode: drop the optional `# Title` H1, take the first
            // non-empty paragraph as the abstract, and the remainder (trimmed)
            // as the context body. This is the inverse of `serialize_markdown`.
            let (abstract_text, context_text) = split_abstract_and_context(body);
            content.insert(
                Value::String("abstract".into()),
                Value::String(abstract_text),
            );
            content.insert(Value::String("context".into()), Value::String(context_text));
        }
        map.insert(Value::String("content".into()), Value::Mapping(content));
    } else {
        return Err(ParseError::MalformedFrontmatter(
            "frontmatter is not a mapping".into(),
        ));
    }
    Ok(())
}

/// Drop a single leading `# Title` H1 line (and the blank line that follows it)
/// from a markdown body, returning the remainder. If there is no leading H1 the
/// body is returned unchanged.
fn strip_leading_h1(body: &str) -> &str {
    let trimmed = body.trim_start_matches(['\n', '\r']);
    if let Some(rest) = trimmed.strip_prefix("# ") {
        // Skip to the end of the title line.
        match rest.find('\n') {
            Some(nl) => rest[nl + 1..].trim_start_matches(['\n', '\r']),
            None => "",
        }
    } else {
        trimmed
    }
}

/// Split a context-mode markdown body into `(abstract, context)`:
/// drop an optional leading `# Title` H1, take the first non-empty paragraph as
/// the abstract, and everything after the blank line that follows it as the
/// context body. The inverse of the `serialize_markdown` context layout.
fn split_abstract_and_context(body: &str) -> (String, String) {
    let rest = strip_leading_h1(body);
    // The abstract is the first paragraph: text up to the first blank line.
    match rest.find("\n\n") {
        Some(idx) => {
            let abstract_text = rest[..idx].trim().to_string();
            let context_text = rest[idx + 2..].trim().to_string();
            (abstract_text, context_text)
        }
        None => {
            // No blank line: the whole remainder is the abstract, no context.
            (rest.trim().to_string(), String::new())
        }
    }
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
                step.insert(
                    Value::String("description".into()),
                    Value::String(rest.to_string()),
                );
                steps.push(Value::Mapping(step));
            }
        }
    }
    let mut procedure = Mapping::new();
    procedure.insert(Value::String("steps".into()), Value::Sequence(steps));
    Value::Mapping(procedure)
}

/// Render a `SkillManifest` back to markdown frontmatter form. The body is
/// derived from the populated content mode: `context` → context body,
/// `procedure` → "## Steps" list, `command` → fenced block.
pub fn serialize_markdown(m: &SkillManifest) -> Result<String, ParseError> {
    let frontmatter = serialize_canonical_frontmatter(m)?;
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&frontmatter);
    out.push_str("---\n\n");
    // Single H1 title, then the abstract paragraph, then the body. The abstract
    // is emitted verbatim (trimmed) followed by a blank line so the parser can
    // recover it as the first paragraph. Do NOT emit a duplicate H1.
    out.push_str(&format!("# {}\n\n", m.name));
    out.push_str(m.content.r#abstract.trim());
    out.push('\n');
    if let Some(ctx) = &m.content.context {
        out.push('\n');
        out.push_str(ctx.trim_end());
        out.push('\n');
    } else if let Some(proc) = &m.content.procedure {
        out.push_str("\n## Steps\n");
        for (i, s) in proc.steps.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", i + 1, s.description));
        }
    } else if let Some(cmd) = &m.content.command {
        out.push_str("\n## Command\n\n```\n");
        out.push_str(cmd);
        out.push_str("\n```\n");
    }
    Ok(out)
}

/// Frontmatter is the manifest serialised *without* the `content` field —
/// the content moves into the markdown body.
fn serialize_canonical_frontmatter(m: &SkillManifest) -> Result<String, ParseError> {
    let mut value = serde_yaml_ng::to_value(m)?;
    if let Some(map) = value.as_mapping_mut() {
        map.remove(serde_yaml_ng::Value::String("content".into()));
    }
    Ok(serde_yaml_ng::to_string(&value)?)
}

/// Parse a legacy skill file — pre-M0 markdown with minimal frontmatter
/// (just `name` + `description`). Fills in defaults so the file can be
/// loaded by the new pipeline without rewriting it.
pub fn parse_legacy_markdown(input: &str) -> Result<SkillManifest, ParseError> {
    let (frontmatter, body) = split_frontmatter(input)?;
    let mut value: serde_yaml_ng::Value = serde_yaml_ng::from_str(frontmatter)?;
    let map = value
        .as_mapping_mut()
        .ok_or_else(|| ParseError::LegacyMarkdown("frontmatter is not a mapping".into()))?;
    use serde_yaml_ng::Value;
    map.entry(Value::String("publisher".into()))
        .or_insert(Value::String("human:mur".into()));
    fill_manifest_defaults(&mut value)?;
    inject_content_from_body(&mut value, body)?;
    let m: SkillManifest = serde_yaml_ng::from_value(value)?;
    Ok(m)
}

/// Convenience: parse canonical YAML, serialise back to markdown.
/// Used by `ensure_mur_skill` so built-in yaml skills produce
/// AI-tool-consumable markdown at `SKILL.md`.
pub fn yaml_to_markdown(yaml: &str) -> Result<String, ParseError> {
    let m = parse_canonical(yaml)?;
    serialize_markdown(&m)
}

/// Round-trip integrity guard: serialise the manifest to markdown, parse it
/// back, and confirm the content survived. Used by `mur skill validate` to make
/// silent abstract/context corruption visible. Only the `context` content mode
/// is checked verbatim — procedure/command/note bodies are reconstructed
/// structurally and compared by mode. Returns `Err(message)` describing the
/// drift if the round-trip would alter content.
pub fn roundtrip_check(m: &SkillManifest) -> Result<(), String> {
    let md = serialize_markdown(m).map_err(|e| format!("serialize_markdown: {e}"))?;
    let reparsed = parse_markdown(&md).map_err(|e| format!("parse_markdown: {e}"))?;

    if reparsed.content.r#abstract.trim() != m.content.r#abstract.trim() {
        return Err(format!(
            "abstract differs after round-trip\n  original:  {:?}\n  roundtrip: {:?}",
            m.content.r#abstract, reparsed.content.r#abstract
        ));
    }

    // Context is stored verbatim; compare trimmed to ignore trailing-newline noise.
    let orig_ctx = m.content.context.as_deref().map(str::trim_end);
    let rt_ctx = reparsed.content.context.as_deref().map(str::trim_end);
    if orig_ctx != rt_ctx {
        return Err(format!(
            "context differs after round-trip\n  original:  {orig_ctx:?}\n  roundtrip: {rt_ctx:?}"
        ));
    }

    Ok(())
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
    fn external_claude_style_frontmatter_defaults_mur_fields() {
        // Claude Code plugin SKILL.md: only name + description.
        let md = "---\nname: ponytail\ndescription: Lazy senior dev mode.\n---\n\nBe lazy.\n";
        let m = parse_markdown(md).unwrap();
        assert_eq!(m.version, "0.0.0");
        assert_eq!(m.publisher, "human:import");
        assert!(super::super::validate::validate(&m).is_ok());
    }

    #[test]
    fn markdown_without_frontmatter_fails() {
        let md = "# just a heading\n";
        assert!(matches!(
            parse_markdown(md),
            Err(ParseError::MissingFrontmatter)
        ));
    }

    #[test]
    fn canonical_to_markdown_roundtrips_context() {
        let m = parse_canonical(SAMPLE).unwrap();
        let md = serialize_markdown(&m).unwrap();
        let m2 = parse_markdown(&md).unwrap();
        assert_eq!(m.name, m2.name);
        assert_eq!(m.content.context.is_some(), m2.content.context.is_some());
    }

    #[test]
    fn canonical_to_markdown_roundtrips_workflow() {
        let yaml = r#"
name: w
version: 1.0.0
publisher: human:test
description: d
category: workflow
content:
  abstract: a
  procedure:
    steps:
      - description: First
      - description: Second
"#;
        let m = parse_canonical(yaml).unwrap();
        let md = serialize_markdown(&m).unwrap();
        let m2 = parse_markdown(&md).unwrap();
        let p2 = m2.content.procedure.unwrap();
        assert_eq!(p2.steps.len(), 2);
        assert_eq!(p2.steps[0].description, "First");
    }

    #[test]
    fn legacy_minimal_frontmatter_loads() {
        let md =
            "---\nname: mur-context\ndescription: Background context\n---\n\n# MUR\n\nSome body.\n";
        let m = parse_legacy_markdown(md).unwrap();
        assert_eq!(m.name, "mur-context");
        assert_eq!(m.publisher, "human:mur");
        assert_eq!(m.version, "0.0.0");
        assert!(m.content.context.is_some());
    }

    #[test]
    fn yaml_to_markdown_yields_consumable_md() {
        let md = yaml_to_markdown(SAMPLE).unwrap();
        assert!(md.starts_with("---"), "should start with frontmatter fence");
        assert!(md.contains("# demo-skill"), "should contain heading");
        assert!(md.contains("hello"), "should contain abstract");
        assert!(md.contains("body"), "should contain context body");
    }

    /// A deliberate multi-sentence abstract plus a multi-paragraph context (with
    /// a code fence and a `## Heading`) must survive yaml→md→yaml without loss.
    #[test]
    fn yaml_md_yaml_roundtrip_preserves_abstract_and_context() {
        let yaml = r#"
name: roundtrip-demo
version: 2.3.1
publisher: human:alan
description: A skill exercising lossless round-trip.
category: context
tags: [alpha, beta]
triggers:
  - type: keyword
    pattern: roundtrip
content:
  abstract: |-
    This skill does one specific thing. It is described in two full sentences so
    the truncation bug would be obvious.
  context: |-
    First context paragraph with real prose that should survive verbatim.

    ## A Heading

    Some explanation under the heading.

    ```rust
    fn demo() {
        println!("code fence must survive");
    }
    ```

    Final closing paragraph.
"#;
        let m = parse_canonical(yaml).unwrap();
        let md = serialize_markdown(&m).unwrap();
        let m2 = parse_markdown(&md).unwrap();

        assert_eq!(
            m2.content.r#abstract, m.content.r#abstract,
            "abstract must round-trip exactly"
        );
        assert_eq!(
            m2.content.context, m.content.context,
            "context must round-trip exactly"
        );
        // Other manifest fields survive via frontmatter.
        assert_eq!(m2.version, m.version);
        assert_eq!(m2.publisher, m.publisher);
        assert_eq!(m2.tags, m.tags);
        assert_eq!(m2.triggers.len(), m.triggers.len());
        assert_eq!(m2.triggers[0].pattern, m.triggers[0].pattern);
    }

    /// A hand-authored markdown (title + abstract paragraph + body) parses, then
    /// serializing and parsing again yields an identical manifest (idempotent).
    #[test]
    fn md_yaml_md_roundtrip_stable() {
        let md = r#"---
name: handauthored
version: 1.2.0
publisher: human:alan
description: Hand-authored markdown skill.
category: context
---

# handauthored

This is the abstract. It spans two sentences on purpose.

This is the first body paragraph.

## Details

More body content here, including a list:

- one
- two
"#;
        let m1 = parse_markdown(md).unwrap();
        let md2 = serialize_markdown(&m1).unwrap();
        let m2 = parse_markdown(&md2).unwrap();

        assert_eq!(
            m1.content.r#abstract, m2.content.r#abstract,
            "abstract must be stable across md→yaml→md"
        );
        assert_eq!(
            m1.content.context, m2.content.context,
            "context must be stable across md→yaml→md"
        );
        assert_eq!(m1.name, m2.name);
        assert_eq!(m1.version, m2.version);
        // The abstract must NOT be the bare H1 title (the old truncation bug).
        assert_ne!(m1.content.r#abstract.trim(), "# handauthored");
        assert!(
            m1.content.r#abstract.contains("This is the abstract."),
            "abstract should be the first paragraph, got: {:?}",
            m1.content.r#abstract
        );
    }

    #[test]
    fn roundtrip_check_passes_for_faithful_context_skill() {
        let m = parse_canonical(SAMPLE).unwrap();
        assert!(roundtrip_check(&m).is_ok());
    }

    #[test]
    fn roundtrip_check_passes_for_multiparagraph_abstract_and_context() {
        let yaml = r#"
name: rc-demo
version: 1.0.0
publisher: human:test
description: d
category: context
content:
  abstract: |-
    Sentence one of the abstract. Sentence two of the abstract.
  context: |-
    Para one.

    Para two with a fence:

    ```
    code
    ```
"#;
        let m = parse_canonical(yaml).unwrap();
        assert!(
            roundtrip_check(&m).is_ok(),
            "expected faithful round-trip, got: {:?}",
            roundtrip_check(&m)
        );
    }
}
