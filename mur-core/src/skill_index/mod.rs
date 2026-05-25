pub mod text;

use crate::store::embedding::EmbeddingConfig;
use crate::store::vector::{EmbeddedChunk, VectorStore};
use chrono::Utc;
use mur_common::skill::manifest::{Skill, SkillManifest};

pub const SKILL_SOURCE_ID: &str = "skill";

/// Embed and upsert from a full `Skill` wrapper.
pub async fn embed_and_upsert(
    skill: &Skill,
    config: &EmbeddingConfig,
    store: &dyn VectorStore,
) -> anyhow::Result<usize> {
    embed_manifest_and_upsert(
        &skill.manifest,
        &skill.manifest.name,
        &skill.manifest.version,
        config,
        store,
    )
    .await
}

/// Embed and upsert from a `SkillManifest` — useful during install when we
/// don't have a full `Skill` wrapper yet.
pub async fn embed_manifest_and_upsert(
    m: &SkillManifest,
    name: &str,
    version: &str,
    config: &EmbeddingConfig,
    store: &dyn VectorStore,
) -> anyhow::Result<usize> {
    let text_str = text::embed_manifest(m);
    let vec = crate::store::embedding::embed(&text_str, config).await?;
    let dims = vec.len();

    let chunk = EmbeddedChunk {
        chunk_id: format!("skill:{name}:{version}"),
        source_id: SKILL_SOURCE_ID.into(),
        external_id: name.to_string(),
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
