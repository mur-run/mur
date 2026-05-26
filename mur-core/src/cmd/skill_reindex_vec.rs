//! `mur skill reindex-vec` — rebuild the skill embedding index.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};

use crate::skill_index::{SKILL_SOURCE_ID, embed_manifest_and_upsert};
use crate::store::embedding::EmbeddingConfig;

pub async fn cmd_reindex_vec(home: &Path, filter: Option<&str>, prune: bool) -> Result<()> {
    let cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
    let embed_config = EmbeddingConfig::from_config(&cfg);
    let index_dir = home.join("lance");
    let store = crate::store::vector::factory::get_vector_store(&cfg, &index_dir)
        .await
        .context("opening vector store")?;

    let installed_names: Vec<String> =
        mur_common::skill::local::list_installed(home).map_err(|e| anyhow::anyhow!("{e}"))?;

    // Filter by exact name match (glob support TBD if demand arises).
    let to_index: Vec<String> = if let Some(pat) = filter {
        installed_names.into_iter().filter(|n| n == pat).collect()
    } else {
        installed_names
    };

    let installed_set: HashSet<String> = to_index.iter().cloned().collect();

    // Prune embeddings for skills no longer installed.
    if prune {
        let indexed = store.list_external_ids(SKILL_SOURCE_ID).await?;
        let stale: Vec<String> = indexed
            .into_iter()
            .filter(|n| !installed_set.contains(n))
            .collect();
        if !stale.is_empty() {
            store
                .delete_by_external_ids(SKILL_SOURCE_ID, &stale)
                .await?;
            println!("Pruned {} stale skill embeddings", stale.len());
        }
    }

    let mut indexed = 0u64;
    let mut failed = 0u64;

    for name in &to_index {
        match mur_common::skill::local::load_installed(home, name) {
            Ok(manifest) => {
                match embed_manifest_and_upsert(
                    &manifest,
                    name,
                    &manifest.version,
                    &embed_config,
                    &*store,
                )
                .await
                {
                    Ok(_dims) => {
                        println!("Indexed {name}");
                        indexed += 1;
                    }
                    Err(e) => {
                        eprintln!("Failed to index {name}: {e}");
                        failed += 1;
                    }
                }
            }
            Err(e) => {
                eprintln!("Failed to load {name}: {e}");
                failed += 1;
            }
        }
    }

    println!("Done: {indexed} indexed, {failed} failed");
    Ok(())
}
