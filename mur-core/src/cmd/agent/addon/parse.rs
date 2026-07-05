//! Claude plugin → MUR primitive converters (pure, no I/O).

use std::collections::BTreeMap;

use anyhow::Result;
use serde::Deserialize;

use mur_common::skill::manifest::{SkillScope, Visibility};
use mur_common::skill::{
    Category, Content, Priority, Provenance, SkillManifest, Trigger, TriggerKind,
};

/// `plugin.json` (only the fields we consume).
#[derive(Debug, Clone, Deserialize)]
pub struct PluginJson {
    pub name: String,
    #[serde(default)]
    pub version: String,
    // Deserialized for completeness; displayed in future UX (Phase 3).
    #[allow(dead_code)]
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: Option<Author>,
}

/// Claude `author` is either a bare string or `{ "name": ... }`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Author {
    Name(String),
    Obj { name: String },
}

pub fn publisher_of(p: &PluginJson) -> String {
    match &p.author {
        Some(Author::Name(s)) => s.clone(),
        Some(Author::Obj { name }) => name.clone(),
        None => p.name.clone(),
    }
}

pub fn manifest_version(p: &PluginJson) -> String {
    if p.version.is_empty() {
        "0.0.0".to_string()
    } else {
        p.version.clone()
    }
}

/// Build a `SkillManifest` with MUR-import defaults; callers vary the shape.
#[allow(clippy::too_many_arguments)]
fn base_manifest(
    name: String,
    publisher: String,
    description: String,
    category: Category,
    content: Content,
    triggers: Vec<Trigger>,
    tags: Vec<String>,
    version: String,
) -> SkillManifest {
    SkillManifest {
        name,
        version,
        publisher,
        description,
        category,
        scope: SkillScope::User,
        visibility: Visibility::default(),
        origin: None,
        origin_version: None,
        origin_hash: None,
        fleet: None,
        team: None,
        governance: None,
        project: None,
        provenance: Provenance::Hybrid, // LLM-authored, human-reviewed source
        hosts: Vec::new(),
        content,
        requires: Vec::new(),
        tags,
        triggers,
        priority: Priority::default(),
        evolution_log: Vec::new(),
        transfer_chain: Vec::new(),
        mcp_requirements: Vec::new(),
        updated_at: chrono::Utc::now(),
    }
}

/// Parsed SKILL.md: YAML frontmatter `name`/`description` + markdown body.
#[derive(Debug, Clone, Default)]
pub struct SkillMd {
    pub name: String,
    pub description: String,
    pub body: String,
}

/// Find the offset of the REAL closing frontmatter fence in `rest` (the text
/// after the opening `---`).  A real closing fence is `\n---\n` or `\n---` at
/// end-of-string.  A bare `\n---` followed by more content (e.g. a Markdown
/// horizontal-rule) is NOT a closing fence and is skipped.
fn find_closing_fence(rest: &str) -> Option<usize> {
    let needle = "\n---";
    let mut search_from = 0;
    while let Some(rel) = rest[search_from..].find(needle) {
        let abs = search_from + rel;
        let after = abs + needle.len();
        // Real closing fence: ends with \n or is at end-of-string.
        if rest[after..].starts_with('\n') || after == rest.len() {
            return Some(abs);
        }
        // Not a real fence — keep scanning past this position.
        search_from = after;
    }
    None
}

/// Split a SKILL.md into frontmatter fields + body. A leading `---` ... `---`
/// fence is parsed as YAML; absent fence => whole file is the body.
pub fn parse_skill_md(raw: &str) -> SkillMd {
    if let Some(rest) = raw.strip_prefix("---")
        && let Some(end) = find_closing_fence(rest)
    {
        let fm = &rest[..end];
        // Body begins after the closing fence line (skip "\n---" then the
        // trailing newline if present, so the body starts at the next line).
        let after = &rest[end + "\n---".len()..];
        let body = after.strip_prefix('\n').unwrap_or(after).to_string();
        let (mut name, mut description) = (String::new(), String::new());
        if let Ok(v) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(fm) {
            name = v
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            description = v
                .get("description")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
        }
        return SkillMd {
            name,
            description,
            body,
        };
    }
    SkillMd {
        body: raw.to_string(),
        ..Default::default()
    }
}

/// Convert a Claude `skills/<dir>/SKILL.md` to a MUR `SkillManifest`.
/// Freeform instructions => `Category::Context` with `content.context = body`
/// (matches category↔mode validation; the spec's "procedure" wording is loose —
/// a Claude skill is a context blob, not a structured procedure). Abstract from
/// `description`. Triggers: Keyword(name) + Manual; NO SessionStart auto-inject.
pub fn skill_md_to_manifest(dir_name: &str, raw: &str, p: &PluginJson) -> SkillManifest {
    let parsed = parse_skill_md(raw);
    let name = if parsed.name.is_empty() {
        dir_name.to_string()
    } else {
        parsed.name
    };
    let content = Content {
        r#abstract: parsed.description.clone(),
        context: Some(parsed.body),
        procedure: None,
        command: None,
        note: None,
    };
    let triggers = vec![
        Trigger {
            kind: TriggerKind::Keyword,
            pattern: Some(name.clone()),
        },
        Trigger {
            kind: TriggerKind::Manual,
            pattern: None,
        },
    ];
    base_manifest(
        name,
        publisher_of(p),
        parsed.description,
        Category::Context,
        content,
        triggers,
        p_tags(p),
        manifest_version(p),
    )
}

/// `commands/<name>.toml` (only `prompt`/`description`).
#[derive(Debug, Clone, Deserialize)]
pub struct CommandToml {
    pub prompt: String,
    #[serde(default)]
    pub description: String,
}

/// Convert a Claude slash-command TOML to a `Category::Command` skill.
/// `content.command = prompt` (`{{args}}` preserved); trigger `Command(/name)`.
/// Runtime semantics: instruction injection, no dispatcher (spec §6).
pub fn command_to_manifest(
    cmd_name: &str,
    toml_src: &str,
    p: &PluginJson,
) -> Result<SkillManifest> {
    let parsed: CommandToml = toml::from_str(toml_src)?;
    let description = if parsed.description.is_empty() {
        format!("Command: /{cmd_name}")
    } else {
        parsed.description
    };
    let content = Content {
        r#abstract: description.clone(),
        context: None,
        procedure: None,
        command: Some(parsed.prompt),
        note: None,
    };
    let triggers = vec![Trigger {
        kind: TriggerKind::Command,
        pattern: Some(format!("/{cmd_name}")),
    }];
    Ok(base_manifest(
        cmd_name.to_string(),
        publisher_of(p),
        description,
        Category::Command,
        content,
        triggers,
        p_tags(p),
        manifest_version(p),
    ))
}

fn p_tags(_p: &PluginJson) -> Vec<String> {
    // plugin.json carries no tags in the local-dir first cut; empty for now.
    Vec::new()
}

/// `.mcp.json` (the `mcpServers` map).
#[derive(Debug, Clone, Deserialize)]
pub struct McpJson {
    #[serde(rename = "mcpServers", default)]
    pub mcp_servers: BTreeMap<String, McpServerJson>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpServerJson {
    /// stdio servers carry a launch command; `type: "http"`/`sse` servers
    /// (e.g. github, linear) carry a `url` instead and leave this empty. The
    /// importer pins stdio binaries by sha256 and cannot pin a remote server,
    /// so it skips empty-command entries (see import.rs).
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Env keys are surfaced as a notice only; nothing secret enters the
    /// converted McpServerEntry or agent profile. See Task 4 for the notice
    /// path.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// Parse a `.mcp.json` in either shape seen in the wild:
///   - wrapped  — `{"mcpServers": {"<name>": {...}}}` (project/user `.mcp.json`)
///   - bare map — `{"<name>": {...}}`                 (Claude plugin `.mcp.json`)
///
/// Every stock Claude marketplace plugin uses the bare-map form, so the old
/// `mcpServers`-only parse silently imported zero servers from them.
pub fn parse_mcp_json(src: &str) -> Result<McpJson> {
    let root: serde_json::Value = serde_json::from_str(src)?;
    let servers = match root.get("mcpServers") {
        Some(wrapped) => wrapped.clone(),
        None => root,
    };
    Ok(McpJson {
        mcp_servers: serde_json::from_value(servers)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::skill::{Category, TriggerKind};

    fn plugin() -> PluginJson {
        PluginJson {
            name: "superpowers".into(),
            version: "6.0.3".into(),
            description: "test plugin".into(),
            author: Some(Author::Name("Acme".into())),
        }
    }

    #[test]
    fn parse_skill_md_splits_frontmatter_and_body() {
        let md = "---\nname: brainstorm\ndescription: helps you think\n---\nDo the thing.\n";
        let r = parse_skill_md(md);
        assert_eq!(r.name, "brainstorm");
        assert_eq!(r.description, "helps you think");
        assert_eq!(r.body, "Do the thing.\n");
    }

    #[test]
    fn skill_md_to_manifest_is_sandboxable_context() {
        let md = "---\nname: brainstorm\ndescription: helps you think\n---\nBody.\n";
        let m = skill_md_to_manifest("brainstorm-dir", md, &plugin());
        assert_eq!(m.name, "brainstorm");
        assert_eq!(m.publisher, "Acme");
        assert!(matches!(m.category, Category::Context));
        assert_eq!(m.content.r#abstract, "helps you think");
        assert_eq!(m.content.context.as_deref(), Some("Body.\n"));
        assert!(m.content.procedure.is_none() && m.content.command.is_none());
        // Keyword + Manual triggers, NO SessionStart (no auto-inject).
        assert!(m.triggers.iter().any(|t| t.kind == TriggerKind::Keyword));
        assert!(m.triggers.iter().any(|t| t.kind == TriggerKind::Manual));
        assert!(
            !m.triggers
                .iter()
                .any(|t| t.kind == TriggerKind::SessionStart)
        );
    }

    #[test]
    fn skill_md_falls_back_to_dir_name_when_frontmatter_missing() {
        let m = skill_md_to_manifest("my-dir", "no frontmatter here", &plugin());
        assert_eq!(m.name, "my-dir");
    }

    #[test]
    fn command_to_manifest_is_command_category() {
        let toml_src = "prompt = \"Review the diff: {{args}}\"\ndescription = \"code review\"\n";
        let m = command_to_manifest("review", toml_src, &plugin()).unwrap();
        assert_eq!(m.name, "review");
        assert!(matches!(m.category, Category::Command));
        assert_eq!(
            m.content.command.as_deref(),
            Some("Review the diff: {{args}}")
        );
        assert!(m.triggers.iter().any(|t| t.kind == TriggerKind::Command));
    }

    #[test]
    fn parse_mcp_json_reads_servers_and_env() {
        let src = r#"{"mcpServers":{"weather":{"command":"weather-mcp","args":["--port","9"],"env":{"API_KEY":"x"}}}}"#;
        let j = parse_mcp_json(src).unwrap();
        let s = j.mcp_servers.get("weather").unwrap();
        assert_eq!(s.command, "weather-mcp");
        assert_eq!(s.args, vec!["--port", "9"]);
        assert!(s.env.contains_key("API_KEY"));
    }

    #[test]
    fn parse_mcp_json_reads_bare_map_form() {
        // Every stock Claude plugin .mcp.json is a bare server map with no
        // `mcpServers` wrapper (e.g. context7, serena, playwright).
        let src = r#"{"context7":{"command":"npx","args":["-y","@upstash/context7-mcp"]}}"#;
        let j = parse_mcp_json(src).unwrap();
        let s = j.mcp_servers.get("context7").unwrap();
        assert_eq!(s.command, "npx");
        assert_eq!(s.args, vec!["-y", "@upstash/context7-mcp"]);
    }

    #[test]
    fn parse_mcp_json_tolerates_remote_http_server() {
        // type:"http" servers (github, linear) carry a url, not a command —
        // they must parse (empty command) so the importer can skip them
        // instead of the whole parse blowing up.
        let src = r#"{"github":{"type":"http","url":"https://api.example/mcp/"}}"#;
        let j = parse_mcp_json(src).unwrap();
        let s = j.mcp_servers.get("github").unwrap();
        assert!(s.command.is_empty());
    }

    #[test]
    fn parse_skill_md_preserves_body_horizontal_rule() {
        // A body `---` is a Markdown HR, NOT a closing frontmatter fence.
        // It must survive intact in the returned body.
        let md = "---\nname: divider-test\ndescription: two sections\n---\nFirst paragraph.\n\n---\n\nSecond paragraph.\n";
        let r = parse_skill_md(md);
        assert_eq!(r.name, "divider-test");
        assert_eq!(r.description, "two sections");
        // Both paragraphs AND the --- divider must be present.
        assert!(
            r.body.contains("First paragraph."),
            "missing first paragraph"
        );
        assert!(r.body.contains("---"), "body --- divider was dropped");
        assert!(
            r.body.contains("Second paragraph."),
            "missing second paragraph; body was truncated at HR"
        );
    }
}
