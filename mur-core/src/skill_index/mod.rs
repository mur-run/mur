pub mod text;

use crate::store::embedding::EmbeddingConfig;
use crate::store::vector::{EmbeddedChunk, VectorStore};
use chrono::Utc;
use mur_common::skill::manifest::Skill;

pub const SKILL_SOURCE_ID: &str = "skill";

pub async fn embed_and_upsert(
    skill: &Skill,
    config: &EmbeddingConfig,
    store: &dyn VectorStore,
) -> anyhow::Result<usize> {
    let text_str = text::embed_text(skill);
    let vec = crate::store::embedding::embed(&text_str, config).await?;
    let dims = vec.len();

    let chunk = EmbeddedChunk {
        chunk_id: format!("skill:{}:{}", skill.manifest.name, skill.manifest.version),
        source_id: SKILL_SOURCE_ID.into(),
        external_id: skill.manifest.name.clone(),
        ordinal: 0,
        text: text_str,
        heading_path: vec![],
        char_range: (0, 0),
        updated_at: Utc::now(),
        embedding: vec,
    };
    store.upsert(&[chunk]).await?;
    Ok(dims)
}

pub async fn delete(skill_name: &str, store: &dyn VectorStore) -> anyhow::Result<()> {
    store
        .delete_by_external_ids(SKILL_SOURCE_ID, &[skill_name.to_string()])
        .await
}
