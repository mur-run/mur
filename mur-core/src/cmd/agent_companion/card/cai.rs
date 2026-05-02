//! Character.AI scraped JSON → MurCard normalizer.
//!
//! Roadmap §4.4: `definition` → `description`, `greeting` →
//! `first_mes`, `default_voice_id` → `extensions.mur.voice.voice_id`
//! with `provider: "character-ai"`. Cards without `default_voice_id`
//! get `provider: "none"`.
//!
//! `external_id`, when present, is recorded in `source` as the
//! canonical character.ai URL so the user can trace provenance.

#![allow(dead_code)]

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::character_card::extensions::{MurExt, VoiceExt};
use crate::character_card::schema::{CardData, Extensions, MurCard};

#[derive(Debug, Deserialize)]
struct CaiScrape {
    name: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    definition: String,
    #[serde(default)]
    greeting: String,
    #[serde(default)]
    default_voice_id: Option<String>,
    #[serde(default)]
    external_id: Option<String>,
}

pub fn normalize_cai(json: &str) -> Result<MurCard> {
    let scrape: CaiScrape = serde_json::from_str(json).context("parse c.ai JSON")?;

    let voice = match scrape.default_voice_id {
        Some(id) if !id.is_empty() => VoiceExt {
            provider: "character-ai".into(),
            voice_id: id,
            speed: 1.0,
        },
        _ => VoiceExt {
            provider: "none".into(),
            voice_id: String::new(),
            speed: 1.0,
        },
    };

    let mut source = Vec::new();
    if let Some(eid) = scrape.external_id.as_deref() {
        source.push(format!("https://character.ai/character/{eid}"));
    }

    let mut data = CardData {
        name: scrape.name,
        description: scrape.definition,
        first_mes: scrape.greeting,
        ..Default::default()
    };
    if !scrape.title.is_empty() {
        data.scenario = scrape.title;
    }
    data.source = source;

    let extensions = Extensions {
        mur: Some(MurExt {
            schema_version: 1,
            voice: Some(voice),
            avatar: None,
            relationship: None,
            first_memory: None,
            companion: None,
            provenance: None,
        }),
    };

    Ok(MurCard {
        spec: "murcard_v1".into(),
        spec_version: "1.0".into(),
        data,
        extensions: Some(extensions),
        ccv3_passthrough: Default::default(),
    })
}
