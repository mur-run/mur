//! Onboarding wizard for `mur agent companion init`.
//!
//! Phase 1.1 — non-interactive (`--answers <file>`) path and
//! interactive 3-step `dialoguer`-based wizard.

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use dialoguer::{Input, Select};
use fs2::FileExt;
use mur_common::agent::{
    AgentProfile, FirstMemory, OnboardingState, ProactiveTier, VoiceOverrides, default_locale,
};
use mur_common::companion::{Formality, Relationship};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use super::util::{atomic_write_json, atomic_write_yaml};

/// On-disk shape of the `--answers <file>` YAML payload.
///
/// D2 onboarding (Phase 1.1 §M2.3.1) extends the original 3-step shape
/// with three optional fields:
/// * `agent_display_name` — what the user names *the agent*
/// * `first_memory` — the seed fact recorded as a `FirstMemory`
/// * `proactive_tier` — three-layer permission selector mapped onto
///   `companion.{enabled, rhythm.enabled, proactive.enabled}` via
///   [`ProactiveTier::apply`]
///
/// All three are `#[serde(default)]` so legacy 3-step YAML files keep
/// deserialising unchanged.
#[derive(Debug, Deserialize)]
struct Answers {
    locale: String,
    name_for_user: String,
    relationship: Relationship,
    #[serde(default)]
    formality: Option<Formality>,
    #[serde(default)]
    extra_instructions: Option<String>,
    #[serde(default)]
    agent_display_name: Option<String>,
    #[serde(default)]
    first_memory: Option<String>,
    #[serde(default)]
    proactive_tier: Option<ProactiveTier>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    first_memory: Option<&'a FirstMemory>,
}

pub async fn run(name: &str, answers: Option<PathBuf>, re_init: bool) -> Result<()> {
    let answers = match answers {
        Some(path) => load_answers(&path)?,
        None => run_wizard()?,
    };

    let agent_dir = super::util::agent_home_for(name)?;

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
    profile.companion.locale = answers.locale.clone();
    profile.companion.relationship = answers.relationship.clone();
    profile.companion.voice_overrides = VoiceOverrides {
        name_for_user: Some(answers.name_for_user.clone()),
        formality: answers.formality.clone(),
        extra_instructions: answers.extra_instructions.clone(),
    };
    let first_memory = answers.first_memory.as_ref().map(|s| FirstMemory {
        text: s.clone(),
        established_at: now,
    });
    profile.companion.onboarding = OnboardingState {
        completed_at: Some(now),
        version: 1,
        agent_display_name: answers.agent_display_name.clone(),
        first_memory: first_memory.clone(),
    };
    // Three-tier proactive: default to WarmOnly when missing — that's the
    // 5-step UX default and matches the prior behaviour of unconditionally
    // setting `companion.enabled = true` while leaving rhythm + proactive
    // dormant. `apply` is the single source of truth for the three-bool
    // mapping (Spec §4.2 step 5 / `ProactiveTier`).
    let tier = answers.proactive_tier.unwrap_or(ProactiveTier::WarmOnly);
    tier.apply(&mut profile.companion);

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
        first_memory: first_memory.as_ref(),
    };
    atomic_write_json(&rel_path, &rel).with_context(|| format!("write {}", rel_path.display()))?;

    println!("Companion mode enabled for {name}.");
    println!(
        "Run `mur agent companion proactive enable {name}` when you're ready for occasional check-ins."
    );

    drop(lock);
    Ok(())
}

fn load_answers(path: &Path) -> Result<Answers> {
    let s = fs::read_to_string(path).with_context(|| format!("read answers {}", path.display()))?;
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
        // M2.3.2 will collect these in the interactive wizard. Until then
        // the 3-step path leaves them unset, matching pre-D2 behaviour.
        agent_display_name: None,
        first_memory: None,
        proactive_tier: None,
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
