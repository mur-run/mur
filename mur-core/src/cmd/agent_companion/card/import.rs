//! Card import orchestrator: detect format → normalize → write to
//! `~/.mur/agents/<name>/inbox/cards/<id>.murcard.yaml` + sidecar
//! meta `<id>.meta.json`.
//!
//! Imports NEVER modify `companion/`. The user runs `card accept`
//! (M4.6) to promote a card from inbox → applied. This mirrors the
//! `mur drafts` quarantine pattern.

#![allow(dead_code)]

use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::path::{Path, PathBuf};
use ulid::Ulid;

use super::{cai, png};
use crate::character_card::schema::MurCard;
use crate::character_card::signing::{VerifyOutcome, verify_card};

#[derive(Debug, Serialize)]
pub struct ImportResult {
    pub id: String,
    pub path: PathBuf,
    pub trust: String,
}

#[derive(Debug, Serialize)]
struct InboxMeta<'a> {
    id: &'a str,
    import_trust: &'a str,
    imported_at: chrono::DateTime<chrono::Utc>,
    original_filename: String,
}

pub async fn import_card(agent: &str, source_path: &Path) -> Result<ImportResult> {
    let agent_home = super::super::util::agent_home_for(agent)?;
    let inbox_dir = agent_home.join("inbox/cards");
    std::fs::create_dir_all(&inbox_dir)
        .with_context(|| format!("create {}", inbox_dir.display()))?;

    let bytes =
        std::fs::read(source_path).with_context(|| format!("read {}", source_path.display()))?;
    let card = detect_and_normalize(&bytes)?;

    let trust = match verify_card(&card) {
        Ok(VerifyOutcome::Signed) => "signed",
        Ok(VerifyOutcome::Unsigned) => "unsigned",
        Err(_) => "failed",
    };

    let id = Ulid::new().to_string();
    let yaml_path = inbox_dir.join(format!("{id}.murcard.yaml"));
    let yaml = serde_yaml_ng::to_string(&card).context("serialize card")?;
    std::fs::write(&yaml_path, yaml).context("write card yaml")?;

    let meta = InboxMeta {
        id: &id,
        import_trust: trust,
        imported_at: chrono::Utc::now(),
        original_filename: source_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string(),
    };
    let meta_path = inbox_dir.join(format!("{id}.meta.json"));
    std::fs::write(&meta_path, serde_json::to_string_pretty(&meta)?).context("write meta")?;

    Ok(ImportResult {
        id,
        path: yaml_path,
        trust: trust.into(),
    })
}

fn detect_and_normalize(bytes: &[u8]) -> Result<MurCard> {
    if bytes.starts_with(&[0x89, 0x50, 0x4e, 0x47]) {
        let json = png::extract_card_json(bytes)?;
        if json.contains("\"chara_card_v3\"") || json.contains("\"spec\"") {
            return png::normalize_v3(&json);
        }
        return png::normalize_v2(&json);
    }
    if let Ok(card) = serde_yaml_ng::from_slice::<MurCard>(bytes) {
        return Ok(card);
    }
    let s = std::str::from_utf8(bytes).context("non-UTF-8 input")?;
    if let Ok(card) = cai::normalize_cai(s) {
        return Ok(card);
    }
    bail!("unrecognized card format (expected PNG, .murcard.yaml, or c.ai JSON)")
}
