//! `card list` — read pending cards from `inbox/cards/`.
//!
//! Returns a vector of `InboxEntry` rows the CLI renders with id +
//! trust marker + import timestamp + display name.

#![allow(dead_code)]

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::character_card::schema::MurCard;

#[derive(Debug, Clone, Serialize)]
pub struct InboxEntry {
    pub id: String,
    pub name: String,
    pub trust: String,
    pub imported_at: String,
}

#[derive(Deserialize)]
struct InboxMetaIn {
    id: String,
    import_trust: String,
    imported_at: chrono::DateTime<chrono::Utc>,
    #[allow(dead_code)]
    original_filename: String,
}

pub fn list_inbox(agent: &str) -> Result<Vec<InboxEntry>> {
    let dir = super::super::util::agent_home_for(agent)?.join("inbox/cards");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let p = entry?.path();
        if p.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let meta_str =
            std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
        let meta: InboxMetaIn = serde_json::from_str(&meta_str)?;
        let yaml_path = dir.join(format!("{}.murcard.yaml", meta.id));
        let card: MurCard = serde_yaml_ng::from_str(&std::fs::read_to_string(&yaml_path)?)?;
        out.push(InboxEntry {
            id: meta.id,
            name: card.data.name,
            trust: meta.import_trust,
            imported_at: meta.imported_at.to_rfc3339(),
        });
    }
    out.sort_by(|a, b| a.imported_at.cmp(&b.imported_at));
    Ok(out)
}
