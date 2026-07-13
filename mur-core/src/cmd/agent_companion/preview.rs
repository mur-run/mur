//! `mur agent companion preview <name> --situation <s> [--no-llm]`
//!
//! Read-only preview of a companion message.  NEVER writes to inbox / ledger /
//! bandit-state.  The only side-effect is stdout.

use anyhow::{Context, Result};
use clap::Args;
use mur_agent_runtime::companion::voice::{VoiceInput, compose_in_memory};
use mur_agent_runtime::llm::{LlmClient, LlmRequest, RichMessage};
use mur_common::agent::AgentProfile;
use mur_common::companion::Situation;
use mur_common::companion::content_seed::{SituationFile, TemplateSeed, all_seeds, parse};
use serde::Deserialize;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

// ─── CLI types ────────────────────────────────────────────────────────────────

#[derive(Args, Debug)]
pub struct PreviewArgs {
    /// Agent name.
    pub name: String,
    /// Situation slug: morning_greeting | gentle_check_in | share_quote | share_link.
    #[arg(long)]
    pub situation: String,
    /// Skip LLM invocation; print voice.md + prompt_seed only.
    #[arg(long)]
    pub no_llm: bool,
}

// ─── Entry point ──────────────────────────────────────────────────────────────

pub async fn run(args: PreviewArgs) -> Result<()> {
    let agent_home = super::util::agent_home_for(&args.name)?;
    run_at(
        &agent_home,
        &args.situation,
        args.no_llm,
        &mut std::io::stdout(),
    )
    .await
}

// ─── Path-taking implementation (also used by tests) ─────────────────────────

/// Core logic, split out for testability.
pub(crate) async fn run_at(
    agent_home: &Path,
    situation_slug: &str,
    no_llm: bool,
    out: &mut impl Write,
) -> Result<()> {
    let profile = load_profile(agent_home)?;
    let situation = parse_situation_slug(situation_slug)?;

    // M2.3.3 — load first_memory from relationship.json so the rendered body
    // can expand `{{FIRST_MEMORY}}` placeholders. Mirrors the runtime-side
    // `mur_agent_runtime::companion::Companion::new` wiring (M2.2.3) on the
    // CLI preview path. Best-effort: a missing/malformed file collapses
    // first_memory placeholders to empty strings.
    let first_memory = load_relationship_first_memory(agent_home);

    // Compose voice.md in-memory — never written.
    let voice = compose_voice_for_preview(&profile, first_memory.as_deref());

    // Pick deterministic template for preview. When first_memory is set,
    // prefer a template whose `prompt_seed` references `{{FIRST_MEMORY}}` so
    // the seed text actually surfaces in the rendered body — otherwise the
    // first template is picked unchanged.
    let template =
        pick_first_template_for_preview(agent_home, &profile, situation, first_memory.as_deref())?;

    if no_llm {
        return preview_no_llm_inner(out, &voice, &template, &profile, first_memory.as_deref());
    }

    // LLM path.
    let llm = build_llm_client(&profile)?;
    let body = generate_preview(llm, &voice, &template).await?;

    writeln!(out, "# voice.md")?;
    writeln!(out, "{voice}")?;
    writeln!(out)?;
    writeln!(out, "# rendered body (preview — NOT delivered)")?;
    writeln!(out, "{body}")?;
    Ok(())
}

/// Testable no-LLM variant that accepts an explicit agent_home path.
#[cfg(test)]
pub(crate) fn preview_no_llm_at(
    agent_home: &Path,
    situation_slug: &str,
    out: &mut impl Write,
) -> Result<()> {
    let profile = load_profile(agent_home)?;
    let situation = parse_situation_slug(situation_slug)?;
    let first_memory = load_relationship_first_memory(agent_home);
    let voice = compose_voice_for_preview(&profile, first_memory.as_deref());
    let template =
        pick_first_template_for_preview(agent_home, &profile, situation, first_memory.as_deref())?;
    preview_no_llm_inner(out, &voice, &template, &profile, first_memory.as_deref())
}

fn preview_no_llm_inner(
    out: &mut impl Write,
    voice: &str,
    template: &TemplateSeed,
    profile: &AgentProfile,
    first_memory: Option<&str>,
) -> Result<()> {
    let rendered_seed = substitute_prompt_seed(&template.prompt_seed, profile, first_memory);
    writeln!(out, "# voice.md")?;
    writeln!(out, "{voice}")?;
    writeln!(out)?;
    writeln!(out, "# prompt_seed (template `{}`)", template.id)?;
    writeln!(out, "{rendered_seed}")?;
    Ok(())
}

/// Compose voice in-memory with the same placeholder substitutions the
/// runtime applies, so `{{FIRST_MEMORY}}`-bearing voice templates render
/// correctly when previewed.
fn compose_voice_for_preview(profile: &AgentProfile, first_memory: Option<&str>) -> String {
    let formality = profile
        .companion
        .voice_overrides
        .formality
        .as_ref()
        .map(|f| format!("{f:?}").to_lowercase())
        .unwrap_or_default();
    let name_for_user = profile
        .companion
        .voice_overrides
        .name_for_user
        .as_deref()
        .unwrap_or("");
    let extra = profile
        .companion
        .voice_overrides
        .extra_instructions
        .as_deref()
        .unwrap_or("");
    compose_in_memory(VoiceInput {
        relationship: profile.companion.relationship.clone(),
        locale: &profile.companion.locale,
        name_for_user,
        first_memory,
        formality: &formality,
        extra_instructions: extra,
    })
}

/// Apply the same placeholder rules to a `prompt_seed` that the runtime's
/// `voice::apply_placeholders` applies to voice templates. Keeps the
/// substitution surface consistent: the user-facing template-defined tokens
/// (`{{FIRST_MEMORY}}`, `{{FIRST_MEMORY_PARAGRAPH}}`, `{{NAME_FOR_USER}}`)
/// expand here, and unknown tokens are left untouched.
fn substitute_prompt_seed(
    prompt_seed: &str,
    profile: &AgentProfile,
    first_memory: Option<&str>,
) -> String {
    // first_memory FIRST so user-supplied name_for_user can't inject more
    // `{{...}}` tokens that get re-substituted.
    let mut s = match first_memory {
        Some(fm) if !fm.is_empty() => prompt_seed
            .replace("{{FIRST_MEMORY}}", fm)
            .replace("{{FIRST_MEMORY_PARAGRAPH}}", &format!(" {fm}")),
        _ => prompt_seed
            .replace("{{FIRST_MEMORY}}", "")
            .replace("{{FIRST_MEMORY_PARAGRAPH}}", ""),
    };
    let name_for_user = profile
        .companion
        .voice_overrides
        .name_for_user
        .as_deref()
        .unwrap_or("");
    s = s.replace("{{NAME_FOR_USER}}", name_for_user);
    s
}

// ─── relationship.json loader (best-effort) ──────────────────────────────────

/// Minimal projection of `companion/relationship.json` — only the fields the
/// preview path needs. Mirrors the runtime parser in
/// `mur_agent_runtime::companion::RelationshipFile`.
#[derive(Debug, Default, Deserialize)]
struct RelationshipFile {
    #[serde(default)]
    first_memory: Option<FirstMemoryRef>,
}

#[derive(Debug, Deserialize)]
struct FirstMemoryRef {
    text: String,
}

/// Best-effort load of `<agent_home>/companion/relationship.json` for the
/// preview path. Returns `None` when the file is missing, unreadable, or
/// malformed — the preview then renders with `{{FIRST_MEMORY}}` collapsed
/// to empty (same fallback as the runtime).
fn load_relationship_first_memory(agent_home: &Path) -> Option<String> {
    let p = agent_home.join("companion/relationship.json");
    let s = std::fs::read_to_string(&p).ok()?;
    match serde_json::from_str::<RelationshipFile>(&s) {
        Ok(rel) => rel.first_memory.map(|fm| fm.text),
        Err(_) => None,
    }
}

// ─── Template picker ──────────────────────────────────────────────────────────

fn pick_first_template_for_preview(
    agent_home: &Path,
    profile: &AgentProfile,
    situation: Situation,
    first_memory: Option<&str>,
) -> Result<TemplateSeed> {
    let locale = &profile.companion.locale;
    let situation_slug = situation_to_slug(&situation);

    // Try disk first.
    let disk_path = agent_home
        .join("companion/content")
        .join(format!("{situation_slug}.{locale}.yaml"));
    if disk_path.exists() {
        let body = std::fs::read_to_string(&disk_path)
            .with_context(|| format!("read {}", disk_path.display()))?;
        let parsed: SituationFile = serde_yaml_ng::from_str(&body)
            .with_context(|| format!("parse {}", disk_path.display()))?;
        if let Some(t) = pick_template_from_list(parsed.templates, first_memory) {
            return Ok(t);
        }
    }

    // Fall back to embedded seed.
    for (sit, loc, yaml) in all_seeds() {
        if sit == situation && loc == locale {
            let parsed = parse(yaml).context("parse embedded seed")?;
            if let Some(t) = pick_template_from_list(parsed.templates, first_memory) {
                return Ok(t);
            }
        }
    }

    anyhow::bail!("no template available for {situation_slug}.{locale} (preview cannot proceed)");
}

/// Deterministic picker: when `first_memory` is set, prefer the first
/// template whose `prompt_seed` references `{{FIRST_MEMORY}}` so the seed
/// text actually surfaces in the preview output. Falls back to the first
/// template in declaration order — the same shape the picker had before
/// M2.3.3.
fn pick_template_from_list(
    templates: Vec<TemplateSeed>,
    first_memory: Option<&str>,
) -> Option<TemplateSeed> {
    if let Some(fm) = first_memory
        && !fm.is_empty()
    {
        // Two-pass walk so the original Vec order still wins ties when no
        // `{{FIRST_MEMORY}}` template exists for this situation.
        let mut iter = templates.into_iter();
        let mut head: Option<TemplateSeed> = None;
        for t in iter.by_ref() {
            if t.prompt_seed.contains("{{FIRST_MEMORY}}") {
                return Some(t);
            }
            if head.is_none() {
                head = Some(t);
            }
        }
        return head;
    }
    templates.into_iter().next()
}

// ─── LLM client construction ─────────────────────────────────────────────────

fn build_llm_client(profile: &AgentProfile) -> Result<Arc<dyn LlmClient>> {
    use mur_agent_runtime::llm::{
        anthropic::AnthropicClient, ollama::OllamaClient, openai::OpenAiClient,
    };

    let provider = profile.model.provider.as_str();
    let model = profile.model.name.clone();

    match provider {
        "anthropic" => {
            let client = AnthropicClient::from_env(model).map_err(|e| {
                anyhow::anyhow!(
                    "preview requires --no-llm because LLM provider anthropic is not configured: {e}"
                )
            })?;
            Ok(Arc::new(client))
        }
        "openai" => {
            let client = OpenAiClient::from_env(model).map_err(|e| {
                anyhow::anyhow!(
                    "preview requires --no-llm because LLM provider openai is not configured: {e}"
                )
            })?;
            Ok(Arc::new(client))
        }
        "ollama" => {
            let base = std::env::var("OLLAMA_BASE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
            Ok(Arc::new(OllamaClient::new(base, model)))
        }
        other => {
            anyhow::bail!(
                "preview requires --no-llm because LLM provider {other} is not configured: unknown provider"
            );
        }
    }
}

// ─── LLM generation ──────────────────────────────────────────────────────────

async fn generate_preview(
    llm: Arc<dyn LlmClient>,
    voice_md: &str,
    template: &TemplateSeed,
) -> Result<String> {
    let req = LlmRequest {
        messages: vec![
            RichMessage::Text {
                role: "system".to_string(),
                content: voice_md.to_string(),
            },
            RichMessage::Text {
                role: "user".to_string(),
                content: template.prompt_seed.clone(),
            },
        ],
        max_tokens: Some(400),
        temperature: Some(0.7),
        tools: vec![],
        ..Default::default()
    };
    let resp = llm
        .generate(req)
        .await
        .map_err(|e| anyhow::anyhow!("LLM generation failed: {e}"))?;
    Ok(resp.text.trim().to_string())
}

// ─── Slug helpers ─────────────────────────────────────────────────────────────

fn parse_situation_slug(slug: &str) -> Result<Situation> {
    match slug {
        "morning_greeting" => Ok(Situation::MorningGreeting),
        "gentle_check_in" => Ok(Situation::GentleCheckIn),
        "share_quote" => Ok(Situation::ShareQuote),
        "share_link" => Ok(Situation::ShareLink),
        "workflow_nudge" => Ok(Situation::WorkflowNudge),
        other => anyhow::bail!(
            "unknown situation `{other}`; valid values: morning_greeting, gentle_check_in, share_quote, share_link, workflow_nudge"
        ),
    }
}

fn situation_to_slug(s: &Situation) -> &'static str {
    match s {
        Situation::MorningGreeting => "morning_greeting",
        Situation::GentleCheckIn => "gentle_check_in",
        Situation::ShareQuote => "share_quote",
        Situation::ShareLink => "share_link",
        Situation::WorkflowNudge => "workflow_nudge",
    }
}

// ─── Profile loader ───────────────────────────────────────────────────────────

fn load_profile(agent_home: &Path) -> Result<AgentProfile> {
    let profile_path = agent_home.join("profile.yaml");
    let yaml = std::fs::read_to_string(&profile_path)
        .with_context(|| format!("read {}", profile_path.display()))?;
    serde_yaml_ng::from_str(&yaml).with_context(|| format!("parse {}", profile_path.display()))
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    const MINIMAL_PROFILE_WITH_COMPANION: &str = r#"
schema: 1
id: 01JQX4TM8Y9K7VQH6B2N3R5DPF
name: test_agent
display_name: "Test"
version: "0.1.0"
persona:
  category: custom
  description: "Test agent"
  traits: { tone: neutral, risk: cautious, verbosity: low }
sys_prompt_file: "sys_prompt.md"
model: { provider: ollama, name: "llama3.2:3b", params: { temperature: 0.2, max_tokens: 4096 } }
mcp_servers: []
skills: []
transport:
  stdio: true
  socket: { enabled: false, bind: "" }
communication: { accepts_from: ["*"], sends_to: [] }
capabilities: []
entitlements:
  network:
    inbound: { ports: [] }
    outbound: { mode: restricted, allow_hosts: [], protocols: ["tcp"], resolve_dns: { mode: system } }
  filesystem: { read: [], write: [], deny: [] }
  processes: { spawn: { mode: allowlist, allowed: [] } }
  syscalls: { mode: default }
  limits: { memory_mb: 512, file_descriptors: 1024, processes: 32 }
notifications: { on_task_complete: [], on_error: [], on_shutdown: [] }
retry:
  llm: { max_retries: 3, backoff: exponential, initial_delay_ms: 1000, max_delay_ms: 30000, retry_on: [rate_limit, timeout, connection_error] }
  tool: { max_retries: 1, backoff: fixed, initial_delay_ms: 500 }
lifecycle: { restart: on_failure, max_restarts: 3, restart_window_secs: 600, stop_timeout_secs: 15, mcp_required: false }
created_at: "2026-04-29T10:00:00+00:00"
updated_at: "2026-04-29T10:00:00+00:00"
companion:
  enabled: true
  locale: "en-US"
  relationship: friend
"#;

    fn write_minimal_profile_with_companion(dir: &Path, _locale: &str) {
        std::fs::write(dir.join("profile.yaml"), MINIMAL_PROFILE_WITH_COMPANION).unwrap();
    }

    #[test]
    fn no_llm_prints_voice_and_prompt_seed_no_writes() {
        let tmp = TempDir::new().unwrap();
        write_minimal_profile_with_companion(tmp.path(), "en-US");
        let mut buf: Vec<u8> = Vec::new();

        preview_no_llm_at(tmp.path(), "morning_greeting", &mut buf).unwrap();

        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("# voice.md"),
            "expected '# voice.md' in output, got:\n{out}"
        );
        assert!(
            out.contains("# prompt_seed"),
            "expected '# prompt_seed' in output, got:\n{out}"
        );

        // Read-only invariant: no state directories must be created.
        assert!(
            !tmp.path().join("companion/inbox").exists(),
            "companion/inbox must not be created by preview"
        );
        assert!(
            !tmp.path().join("companion/outbox-ledger").exists(),
            "companion/outbox-ledger must not be created by preview"
        );
        assert!(
            !tmp.path().join("companion/bandit-state.json").exists(),
            "companion/bandit-state.json must not be created by preview"
        );
    }

    #[test]
    fn unknown_situation_errors() {
        let tmp = TempDir::new().unwrap();
        write_minimal_profile_with_companion(tmp.path(), "en-US");
        let mut buf: Vec<u8> = Vec::new();
        let result = preview_no_llm_at(tmp.path(), "no_such_situation", &mut buf);
        assert!(result.is_err(), "expected error for unknown situation slug");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("unknown situation"),
            "expected 'unknown situation' in error, got: {msg}"
        );
    }
}
