//! Onboarding wizard for `mur agent companion init`.
//!
//! Phase 1.1 — non-interactive (`--answers <file>`) path and
//! interactive 3-step `dialoguer`-based wizard.

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use dialoguer::{Input, Select};
use fs2::FileExt;
use mur_common::agent::{AgentProfile, OnboardingState, VoiceOverrides, default_locale};
use mur_common::companion::{Formality, Relationship};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

/// On-disk shape of the `--answers <file>` YAML payload.
#[derive(Debug, Deserialize)]
struct Answers {
    locale: String,
    name_for_user: String,
    relationship: Relationship,
    #[serde(default)]
    formality: Option<Formality>,
    #[serde(default)]
    extra_instructions: Option<String>,
}

/// Schema written to `companion/relationship.json`.
#[derive(Debug, Serialize)]
struct RelationshipFile<'a> {
    version: u32,
    name_for_user: &'a str,
    relationship: &'a Relationship,
    locale: &'a str,
    formality: &'a Option<Formality>,
    extra_instructions: &'a Option<String>,
    onboarded_at: chrono::DateTime<Utc>,
}

pub async fn run(name: &str, answers: Option<PathBuf>, re_init: bool) -> Result<()> {
    let answers = match answers {
        Some(path) => load_answers(&path)?,
        None => run_wizard()?,
    };

    let mur_home = resolve_mur_home()?;
    let agent_dir = mur_home.join("agents").join(name);
    if !agent_dir.exists() {
        bail!(
            "agent {name} does not exist; run `mur agent create {name}` first"
        );
    }

    let companion_dir = agent_dir.join("companion");
    fs::create_dir_all(&companion_dir)
        .with_context(|| format!("create {}", companion_dir.display()))?;

    // R11: refuse concurrent init.
    let lock_path = companion_dir.join(".init.lock");
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("open {}", lock_path.display()))?;
    lock.try_lock_exclusive()
        .map_err(|_| anyhow!("another `companion init` is running for this agent"))?;

    let profile_path = agent_dir.join("profile.yaml");
    let profile_str = fs::read_to_string(&profile_path)
        .with_context(|| format!("read {}", profile_path.display()))?;
    let mut profile: AgentProfile = serde_yaml_ng::from_str(&profile_str)
        .with_context(|| format!("parse {}", profile_path.display()))?;

    if !re_init && profile.companion.onboarding.completed_at.is_some() {
        bail!("companion already initialized for {name}; pass --re-init to re-run");
    }

    let now = Utc::now();
    profile.companion.enabled = true;
    profile.companion.locale = answers.locale.clone();
    profile.companion.relationship = answers.relationship.clone();
    profile.companion.voice_overrides = VoiceOverrides {
        name_for_user: Some(answers.name_for_user.clone()),
        formality: answers.formality.clone(),
        extra_instructions: answers.extra_instructions.clone(),
    };
    profile.companion.onboarding = OnboardingState {
        completed_at: Some(now),
        version: 1,
    };
    // proactive + rhythm are deliberately untouched.

    atomic_write_yaml(&profile_path, &profile)
        .with_context(|| format!("write {}", profile_path.display()))?;

    let rel_path = companion_dir.join("relationship.json");
    let rel = RelationshipFile {
        version: 1,
        name_for_user: &answers.name_for_user,
        relationship: &answers.relationship,
        locale: &answers.locale,
        formality: &answers.formality,
        extra_instructions: &answers.extra_instructions,
        onboarded_at: now,
    };
    atomic_write_json(&rel_path, &rel)
        .with_context(|| format!("write {}", rel_path.display()))?;

    println!("Companion mode enabled for {name}.");
    println!(
        "Run `mur agent companion proactive enable {name}` when you're ready for occasional check-ins."
    );

    drop(lock);
    Ok(())
}

fn load_answers(path: &Path) -> Result<Answers> {
    let s = fs::read_to_string(path)
        .with_context(|| format!("read answers {}", path.display()))?;
    serde_yaml_ng::from_str::<Answers>(&s)
        .with_context(|| format!("parse answers {}", path.display()))
}

/// Three-step interactive wizard. Mirrors the `--answers` shape so the
/// downstream atomic-write code path is identical.
fn run_wizard() -> Result<Answers> {
    // Step 1: locale + name.
    let locale: String = Input::new()
        .with_prompt("Language (BCP-47, e.g. zh-TW)")
        .default(default_locale())
        .interact_text()
        .context("read locale")?;
    let name_for_user: String = Input::new()
        .with_prompt("What should I call you?")
        .interact_text()
        .context("read name")?;

    // Step 2: relationship slot + example greeting.
    let choices = ["Friend", "Coach", "Accountability buddy", "Mentor"];
    let idx = Select::new()
        .with_prompt("How should this agent relate to you?")
        .items(&choices)
        .default(0)
        .interact()
        .context("select relationship")?;
    let relationship = match idx {
        0 => Relationship::Friend,
        1 => Relationship::Coach,
        2 => Relationship::AccountabilityBuddy,
        _ => Relationship::Mentor,
    };
    if let Some(example) = example_greeting(&locale, &relationship) {
        println!("{example}");
    }

    // Step 3: earned-permission narrative (print only).
    print_narrative(&locale);

    Ok(Answers {
        locale,
        name_for_user,
        relationship,
        formality: Some(Formality::Casual),
        extra_instructions: Some(String::new()),
    })
}

/// Locale-keyed example greeting. zh-* prints zh-TW phrasing; en-* / fallback
/// prints English. Other locale families return None (we'd rather print
/// nothing than print Chinese to a French user).
fn example_greeting(locale: &str, r: &Relationship) -> Option<&'static str> {
    if locale.starts_with("zh") {
        Some(match r {
            Relationship::Friend => "「嗨，今天好嗎？」",
            Relationship::Coach => "「嗨，目標是什麼？」",
            Relationship::AccountabilityBuddy => "「嗨，今天有什麼想做的嗎？」",
            Relationship::Mentor => "「嗨，最近在思考什麼？」",
        })
    } else if locale.starts_with("en") {
        Some(match r {
            Relationship::Friend => "\"Hey — how's it going?\"",
            Relationship::Coach => "\"Hey. What's the goal?\"",
            Relationship::AccountabilityBuddy => "\"Hi, what's on your plate today?\"",
            Relationship::Mentor => "\"Hi. What's been on your mind?\"",
        })
    } else {
        None
    }
}

fn print_narrative(locale: &str) {
    if locale.starts_with("zh") {
        println!("現在我會更暖和地回應你。");
        println!("如果哪天你想讓我偶爾主動打招呼，跑 `mur agent companion proactive enable`。");
    } else {
        println!("I'll respond more warmly from now on.");
        println!(
            "When you want me to occasionally check in, run `mur agent companion proactive enable`."
        );
    }
}

fn resolve_mur_home() -> Result<PathBuf> {
    if let Some(v) = std::env::var_os("MUR_HOME") {
        return Ok(PathBuf::from(v));
    }
    Ok(dirs::home_dir()
        .ok_or_else(|| anyhow!("no home dir"))?
        .join(".mur"))
}

fn atomic_write_yaml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let body = serde_yaml_ng::to_string(value).context("serialize yaml")?;
    atomic_write_bytes(path, body.as_bytes())
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let body = serde_json::to_string_pretty(value).context("serialize json")?;
    atomic_write_bytes(path, body.as_bytes())
}

fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|s| s.to_str()).unwrap_or("tmp")
    ));
    fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}
