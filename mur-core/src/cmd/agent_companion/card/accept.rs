//! `card accept` — promote one inbox card to applied.
//!
//! 7 steps:
//! 1. Read `<id>.murcard.yaml` + `<id>.meta.json` from inbox.
//! 2. Apply selected fields to `profile.companion`:
//!    - `agent_display_name` ← card.data.name
//!    - `first_memory` ← extensions.mur.first_memory
//!    - `locale` ← extensions.mur.relationship.primary_language
//!    - `enabled` ← true via ProactiveTier::WarmOnly.apply
//!    - `onboarding.completed_at` ← now
//! 3. Write `companion/relationship.json` (runtime-side mirror).
//! 4. Concat all `data.*` strings + character_book entries into a
//!    single blob, hash it, write `telemetry/inputs/<sha>.txt`.
//! 5. Append `ProvenanceEntry { source: "card_import", turn_id: 1 }`
//!    to `telemetry/inputs.jsonl`. M3.8's B0SafetyHook reads this on
//!    the next prompt, wraps the text, raises `after_untrusted_input`.
//! 6. Move `inbox/cards/<id>.{murcard.yaml,meta.json}` to
//!    `inbox/cards/.applied/`.
//!
//! Step 5 is the entire B0 integration. M3.8 stays unchanged — it
//! already wraps any provenance entry on the current turn. The
//! `card_import` source string is just metadata for forensics.
//!
//! NOTE: `mur_common::companion::Relationship` is a closed enum
//! (`friend`/`coach`/`accountability_buddy`/`mentor`) and does not
//! implement `FromStr`, so the card's free-form `relationship.kind`
//! string is NOT mapped onto `profile.companion.relationship`. The
//! existing relationship (default `friend`) is left untouched. M4.7
//! can revisit if a known-mapping helper becomes useful.

#![allow(dead_code)]

use anyhow::{Result, bail};
use chrono::Utc;
use sha2::{Digest, Sha256};

use crate::character_card::schema::MurCard;
use mur_common::agent::{AgentProfile, FirstMemory, OnboardingState, ProactiveTier};
use mur_common::multimodal::{ProvenanceEntry, ProvenanceLedger};

pub async fn accept_card(agent: &str, id: &str) -> Result<()> {
    let agent_home = super::super::util::agent_home_for(agent)?;
    let inbox = agent_home.join("inbox/cards");
    let yaml_path = inbox.join(format!("{id}.murcard.yaml"));
    let meta_path = inbox.join(format!("{id}.meta.json"));
    if !yaml_path.exists() {
        bail!("card {id} not found in inbox");
    }

    let card: MurCard = serde_yaml_ng::from_str(&std::fs::read_to_string(&yaml_path)?)?;

    // 1. Update profile.yaml.
    let profile_path = agent_home.join("profile.yaml");
    let mut profile: AgentProfile =
        serde_yaml_ng::from_str(&std::fs::read_to_string(&profile_path)?)?;

    apply_card_to_profile(&mut profile, &card);
    super::super::util::atomic_write_yaml(&profile_path, &profile)?;

    // 2. Write companion/relationship.json mirror.
    let companion_dir = agent_home.join("companion");
    std::fs::create_dir_all(&companion_dir)?;
    let rel_payload = serde_json::json!({
        "version": 1,
        "name_for_user": profile.companion.voice_overrides.name_for_user
            .clone().unwrap_or_default(),
        "relationship": profile.companion.relationship,
        "locale": profile.companion.locale,
        "first_memory": profile.companion.onboarding.first_memory,
        "onboarded_at": profile.companion.onboarding.completed_at,
    });
    super::super::util::atomic_write_json(&companion_dir.join("relationship.json"), &rel_payload)?;

    // 3. Persist the untrusted card text + provenance entry.
    let untrusted = collect_untrusted_text(&card);
    if !untrusted.is_empty() {
        let mut hasher = Sha256::new();
        hasher.update(untrusted.as_bytes());
        let sha = format!("{:x}", hasher.finalize());

        let inputs_dir = agent_home.join("telemetry/inputs");
        std::fs::create_dir_all(&inputs_dir)?;
        std::fs::write(inputs_dir.join(format!("{sha}.txt")), &untrusted)?;

        let entry = ProvenanceEntry {
            sha256: sha,
            source: "card_import".into(),
            decoder_version: "card_import/v1".into(),
            ocr_engine_version: None,
            turn_id: 1,
            recorded_at: Utc::now(),
        };
        ProvenanceLedger::new(agent_home.join("telemetry/inputs.jsonl")).append(&entry)?;
    }

    // 4. Move inbox files to .applied/.
    let applied = inbox.join(".applied");
    std::fs::create_dir_all(&applied)?;
    if let Some(name) = yaml_path.file_name() {
        std::fs::rename(&yaml_path, applied.join(name))?;
    }
    if meta_path.exists()
        && let Some(name) = meta_path.file_name()
    {
        std::fs::rename(&meta_path, applied.join(name))?;
    }
    Ok(())
}

fn apply_card_to_profile(profile: &mut AgentProfile, card: &MurCard) {
    let now = Utc::now();
    let first_memory = card
        .extensions
        .as_ref()
        .and_then(|e| e.mur.as_ref())
        .and_then(|m| m.first_memory.as_ref())
        .map(|fm| FirstMemory {
            text: fm.text.clone(),
            established_at: fm.established_at,
        });
    profile.companion.onboarding = OnboardingState {
        completed_at: Some(now),
        version: 1,
        agent_display_name: Some(card.data.name.clone()),
        first_memory,
    };
    if let Some(rel) = card
        .extensions
        .as_ref()
        .and_then(|e| e.mur.as_ref())
        .and_then(|m| m.relationship.as_ref())
        && let Some(lang) = rel.primary_language.as_deref()
    {
        profile.companion.locale = lang.into();
    }
    ProactiveTier::WarmOnly.apply(&mut profile.companion);
}

fn collect_untrusted_text(card: &MurCard) -> String {
    let d = &card.data;
    let mut chunks = Vec::new();
    if !d.description.is_empty() {
        chunks.push(format!("description:\n{}", d.description));
    }
    if !d.personality.is_empty() {
        chunks.push(format!("personality:\n{}", d.personality));
    }
    if !d.scenario.is_empty() {
        chunks.push(format!("scenario:\n{}", d.scenario));
    }
    if !d.first_mes.is_empty() {
        chunks.push(format!("first_mes:\n{}", d.first_mes));
    }
    if !d.mes_example.is_empty() {
        chunks.push(format!("mes_example:\n{}", d.mes_example));
    }
    if !d.system_prompt.is_empty() {
        chunks.push(format!("system_prompt:\n{}", d.system_prompt));
    }
    if !d.post_history_instructions.is_empty() {
        chunks.push(format!(
            "post_history_instructions:\n{}",
            d.post_history_instructions
        ));
    }
    if let Some(book) = &d.character_book {
        for entry in &book.entries {
            chunks.push(format!(
                "character_book[{}]:\n{}",
                entry.keys.join(","),
                entry.content
            ));
        }
    }
    chunks.join("\n\n")
}
