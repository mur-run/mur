//! `mur agent companion content add <situation>` — append a template entry to
//! the per-agent content-pool file at `companion/content/<situation>.<locale>.yaml`.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use mur_common::companion::Situation;
use mur_common::companion::content_seed::{SituationFile, TemplateSeed};
use std::path::{Path, PathBuf};

use super::util::{agent_home_for, atomic_write_yaml};

// ─── CLI types ────────────────────────────────────────────────────────────────

#[derive(Args, Debug)]
pub struct ContentArgs {
    #[command(subcommand)]
    pub cmd: ContentCmd,
}

#[derive(Subcommand, Debug)]
pub enum ContentCmd {
    /// Append a new template entry to the situation's content file.
    Add {
        /// Agent name.
        name: String,
        /// Situation slug: morning_greeting | gentle_check_in | share_quote | share_link.
        situation: String,
        /// Locale (BCP-47); defaults to profile.companion.locale.
        #[arg(long)]
        locale: Option<String>,
        /// Read the new entry as YAML from stdin.
        #[arg(long, conflicts_with = "file")]
        from_stdin: bool,
        /// Read the new entry as YAML from a file.
        #[arg(long, conflicts_with = "from_stdin")]
        file: Option<PathBuf>,
    },
}

// ─── Entry point ──────────────────────────────────────────────────────────────

pub async fn run(args: ContentArgs) -> Result<()> {
    match args.cmd {
        ContentCmd::Add {
            name,
            situation,
            locale,
            from_stdin,
            file,
        } => {
            let agent_home = agent_home_for(&name)?;
            run_add_at(
                &agent_home,
                &situation,
                locale.as_deref(),
                from_stdin,
                file.as_deref(),
            )
        }
    }
}

// ─── Path-taking implementation (also used by tests) ──────────────────────────

pub(crate) fn run_add_at(
    agent_home: &Path,
    situation_slug: &str,
    locale: Option<&str>,
    from_stdin: bool,
    file: Option<&Path>,
) -> Result<()> {
    let profile = load_profile(agent_home)?;
    let resolved_locale = locale.unwrap_or(&profile.companion.locale).to_string();
    let situation = parse_situation_slug(situation_slug)?;

    let raw = read_input(from_stdin, file)?;
    // Validate required field presence before typed deserialization (serde
    // defaults would silently hide missing `weight` and `tags`; we demand them).
    validate_required_keys(&raw)?;
    let candidate: TemplateSeed =
        serde_yaml_ng::from_str(&raw).context("parse template entry as YAML")?;
    validate_template_seed_values(&candidate)?;

    let dir = agent_home.join("companion/content");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{situation_slug}.{resolved_locale}.yaml"));

    let mut file_state = if path.exists() {
        let body = std::fs::read_to_string(&path)?;
        serde_yaml_ng::from_str(&body)
            .with_context(|| format!("parse existing {}", path.display()))?
    } else {
        SituationFile {
            situation,
            locale: resolved_locale.clone(),
            templates: Vec::new(),
        }
    };

    // Reject duplicate IDs.
    if file_state.templates.iter().any(|t| t.id == candidate.id) {
        anyhow::bail!(
            "template id `{}` already exists in {}",
            candidate.id,
            path.display()
        );
    }
    file_state.templates.push(candidate);

    atomic_write_yaml(&path, &file_state)?;
    println!("✓ appended to {}", path.display());
    Ok(())
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn load_profile(agent_home: &Path) -> Result<mur_common::agent::AgentProfile> {
    let profile_path = agent_home.join("profile.yaml");
    let yaml = std::fs::read_to_string(&profile_path)
        .with_context(|| format!("read {}", profile_path.display()))?;
    serde_yaml_ng::from_str(&yaml).with_context(|| format!("parse {}", profile_path.display()))
}

fn parse_situation_slug(s: &str) -> Result<Situation> {
    use Situation::*;
    Ok(match s {
        "morning_greeting" => MorningGreeting,
        "gentle_check_in" => GentleCheckIn,
        "share_quote" => ShareQuote,
        "share_link" => ShareLink,
        other => anyhow::bail!(
            "unknown situation `{other}`; expected one of \
             morning_greeting|gentle_check_in|share_quote|share_link"
        ),
    })
}

fn read_input(from_stdin: bool, file: Option<&Path>) -> Result<String> {
    use std::io::Read;
    if from_stdin {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("read stdin")?;
        Ok(buf)
    } else if let Some(p) = file {
        std::fs::read_to_string(p).with_context(|| format!("read {}", p.display()))
    } else {
        anyhow::bail!("one of --from-stdin or --file is required");
    }
}

/// Check that all 7 required keys are present in the raw YAML.
/// Done before typed deserialization so that the error message names the
/// missing field clearly even for fields that have serde defaults
/// (`weight`, `tags`).
fn validate_required_keys(raw: &str) -> Result<()> {
    let v: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(raw).context("parse template entry as YAML")?;
    let map = v
        .as_mapping()
        .ok_or_else(|| anyhow::anyhow!("expected YAML mapping at top level"))?;
    let required = [
        "id",
        "weight",
        "cooldown_days",
        "tags",
        "source",
        "reviewed_by",
        "prompt_seed",
    ];
    for key in required {
        if !map.contains_key(serde_yaml_ng::Value::String(key.to_string())) {
            anyhow::bail!("missing required field `{key}`");
        }
    }
    Ok(())
}

/// Check value-level constraints after typed deserialization succeeds.
fn validate_template_seed_values(seed: &TemplateSeed) -> Result<()> {
    if seed.id.trim().is_empty() {
        anyhow::bail!("`id` must be non-empty");
    }
    if seed.prompt_seed.trim().is_empty() {
        anyhow::bail!("`prompt_seed` must be non-empty");
    }
    Ok(())
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::companion::content_seed::SituationFile;
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

    fn valid_yaml() -> &'static str {
        "id: test-greet-1\nweight: 1.0\ncooldown_days: 7\ntags: [test]\nsource: synthetic\nreviewed_by: test\nprompt_seed: \"warm hello\"\n"
    }

    #[test]
    fn add_creates_new_file_with_one_entry() {
        let tmp = TempDir::new().unwrap();
        write_minimal_profile_with_companion(tmp.path(), "en-US");
        run_add_at(tmp.path(), "morning_greeting", Some("en-US"), false, None)
            .expect_err("should require --from-stdin or --file");

        // Use a temp file for input.
        let input_path = tmp.path().join("entry.yaml");
        std::fs::write(&input_path, valid_yaml()).unwrap();
        run_add_at(
            tmp.path(),
            "morning_greeting",
            Some("en-US"),
            false,
            Some(&input_path),
        )
        .unwrap();

        let path = tmp
            .path()
            .join("companion/content/morning_greeting.en-US.yaml");
        assert!(path.exists());
        let body = std::fs::read_to_string(&path).unwrap();
        let parsed: SituationFile = serde_yaml_ng::from_str(&body).unwrap();
        assert_eq!(parsed.situation, Situation::MorningGreeting);
        assert_eq!(parsed.locale, "en-US");
        assert_eq!(parsed.templates.len(), 1);
        assert_eq!(parsed.templates[0].id, "test-greet-1");
    }

    #[test]
    fn add_appends_to_existing_file() {
        let tmp = TempDir::new().unwrap();
        write_minimal_profile_with_companion(tmp.path(), "en-US");

        let input1 = tmp.path().join("entry1.yaml");
        std::fs::write(&input1, valid_yaml()).unwrap();
        run_add_at(
            tmp.path(),
            "morning_greeting",
            Some("en-US"),
            false,
            Some(&input1),
        )
        .unwrap();

        let yaml2 = valid_yaml().replace("test-greet-1", "test-greet-2");
        let input2 = tmp.path().join("entry2.yaml");
        std::fs::write(&input2, &yaml2).unwrap();
        run_add_at(
            tmp.path(),
            "morning_greeting",
            Some("en-US"),
            false,
            Some(&input2),
        )
        .unwrap();

        let path = tmp
            .path()
            .join("companion/content/morning_greeting.en-US.yaml");
        let parsed: SituationFile =
            serde_yaml_ng::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed.templates.len(), 2);
    }

    #[test]
    fn add_rejects_missing_required_field() {
        let tmp = TempDir::new().unwrap();
        write_minimal_profile_with_companion(tmp.path(), "en-US");

        // Missing prompt_seed.
        let bad = "id: x\nweight: 1.0\ncooldown_days: 7\ntags: []\nsource: s\nreviewed_by: r\n";
        let input = tmp.path().join("bad.yaml");
        std::fs::write(&input, bad).unwrap();
        let result = run_add_at(
            tmp.path(),
            "morning_greeting",
            Some("en-US"),
            false,
            Some(&input),
        );
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("prompt_seed"), "err = {err_msg}");

        let path = tmp
            .path()
            .join("companion/content/morning_greeting.en-US.yaml");
        assert!(
            !path.exists(),
            "file should not have been created on validation failure"
        );
    }

    #[test]
    fn add_rejects_duplicate_id() {
        let tmp = TempDir::new().unwrap();
        write_minimal_profile_with_companion(tmp.path(), "en-US");

        let input = tmp.path().join("entry.yaml");
        std::fs::write(&input, valid_yaml()).unwrap();
        run_add_at(
            tmp.path(),
            "morning_greeting",
            Some("en-US"),
            false,
            Some(&input),
        )
        .unwrap();

        // Second attempt with same id.
        let input2 = tmp.path().join("entry2.yaml");
        std::fs::write(&input2, valid_yaml()).unwrap();
        let result = run_add_at(
            tmp.path(),
            "morning_greeting",
            Some("en-US"),
            false,
            Some(&input2),
        );
        assert!(result.is_err());
        assert!(
            format!("{}", result.unwrap_err()).contains("already exists"),
            "expected 'already exists' error"
        );
    }

    #[test]
    fn add_uses_profile_locale_when_none_given() {
        let tmp = TempDir::new().unwrap();
        // Profile has locale "en-US" — pass locale=None, expect en-US file.
        write_minimal_profile_with_companion(tmp.path(), "en-US");

        let input = tmp.path().join("entry.yaml");
        std::fs::write(&input, valid_yaml()).unwrap();
        run_add_at(
            tmp.path(),
            "morning_greeting",
            None, // no locale override
            false,
            Some(&input),
        )
        .unwrap();

        let path = tmp
            .path()
            .join("companion/content/morning_greeting.en-US.yaml");
        assert!(path.exists(), "expected en-US file from profile locale");
    }

    #[test]
    fn add_rejects_unknown_situation() {
        let tmp = TempDir::new().unwrap();
        write_minimal_profile_with_companion(tmp.path(), "en-US");

        let input = tmp.path().join("entry.yaml");
        std::fs::write(&input, valid_yaml()).unwrap();
        let result = run_add_at(
            tmp.path(),
            "totally_wrong",
            Some("en-US"),
            false,
            Some(&input),
        );
        assert!(result.is_err());
        assert!(
            format!("{}", result.unwrap_err()).contains("unknown situation"),
            "expected 'unknown situation' error"
        );
    }
}
