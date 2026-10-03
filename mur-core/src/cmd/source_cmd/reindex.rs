//! `mur source reindex` and `search` handlers.

use anyhow::{Result, bail};

// ---------- Task 8 handlers ----------

pub(super) async fn reindex(id: &str, vector_backend: Option<&str>) -> Result<()> {
    use crate::sources::adapters::obsidian::ObsidianAdapter;
    use crate::sources::instance::SourceInstanceStore;
    use crate::sources::sync::sync_source;
    use crate::sources::tantivy::TantivyIndex;
    use crate::store::embedding::EmbeddingConfig;
    use crate::store::vector::factory::get_vector_store;
    use anyhow::Context;

    let mut cfg = crate::store::config::load_config()?;
    if let Some(backend) = vector_backend {
        cfg.storage.vector_backend = backend.to_string();
        crate::store::config::save_config(&cfg)?;
        println!("🔧 vector_backend set to {backend}");
    }
    let emb_cfg = EmbeddingConfig::from_config(&cfg);
    let index_path = dirs::home_dir()
        .context("no home dir")?
        .join(".mur")
        .join("index");
    // The connector table is shared by every source, so dropping an
    // unreadable one means the other sources need a reindex too; say so.
    if cfg.storage.vector_backend == "lancedb"
        && crate::store::vector::unreadable::drop_unreadable_at(
            &index_path,
            crate::store::vector::lancedb::SOURCES_TABLE,
        )
        .await?
    {
        eprintln!(
            "note: other sources were in the same table; run `mur source sync --full` for each"
        );
    }
    let vector_store = get_vector_store(&cfg, &index_path).await?;
    let tantivy =
        TantivyIndex::open_or_create(&dirs::home_dir().context("no home dir")?.join(".mur"))?;

    let store = SourceInstanceStore::default_store()?;
    let mut inst = store.load(id)?;

    // Reindex wipes the source before re-adding it; refuse up front when the
    // index cannot take the re-add, or the wipe is all that happens (#1614).
    vector_store
        .check_writable()
        .await
        .context("vector store rejects writes; nothing was deleted")?;
    vector_store.delete_by_source(id).await?;
    tantivy.delete_by_source(id)?;
    inst.sync.last_cursor = None;

    if inst.type_name != "obsidian" {
        bail!(
            "reindex for adapter `{}` arrives in a later sub-milestone",
            inst.type_name
        );
    }
    let adapter = ObsidianAdapter::from_instance(&inst)?;
    println!(
        "↻ reindexing {} on backend `{}`",
        inst.id, cfg.storage.vector_backend
    );
    let report = sync_source(
        &adapter,
        &mut inst,
        &store,
        vector_store,
        &tantivy,
        &emb_cfg,
        true,
    )
    .await?;
    println!(
        "  reindexed {} docs ({} chunks), {} errors",
        report.docs_synced,
        report.chunks_emitted,
        report.errors.len()
    );
    for e in report.errors.iter().take(3) {
        println!("  ! {e}");
    }
    if !report.errors.is_empty() {
        bail!(
            "reindex of `{id}` finished with {} error(s)",
            report.errors.len()
        );
    }
    Ok(())
}

// ---------- Task 15 handlers ----------

pub(super) async fn search(
    query: &str,
    limit: usize,
    source: Option<&str>,
    json: bool,
) -> Result<()> {
    use crate::store::embedding::{EmbeddingConfig, embed};
    use crate::store::vector::{SearchFilter, factory::get_vector_store};
    use anyhow::Context;

    let cfg = crate::store::config::load_config()?;
    let emb_cfg = EmbeddingConfig::from_config(&cfg);
    let index_path = dirs::home_dir()
        .context("no home dir")?
        .join(".mur")
        .join("index");
    let vs = get_vector_store(&cfg, &index_path).await?;

    let qvec = embed(query, &emb_cfg).await.context("embed query")?;

    let filter = SearchFilter {
        source_ids: source.map(|s| vec![s.to_string()]),
        since: None,
    };
    let hits = vs.search(&qvec, limit, &filter).await?;

    if json {
        let j = serde_json::to_string_pretty(
            &hits
                .iter()
                .map(|h| {
                    serde_json::json!({
                        "chunk_id": h.chunk_id,
                        "source_id": h.source_id,
                        "external_id": h.external_id,
                        "score": h.score,
                        "text": h.text,
                        "heading_path": h.heading_path,
                        "updated_at": h.updated_at.to_rfc3339(),
                    })
                })
                .collect::<Vec<_>>(),
        )?;
        println!("{j}");
        return Ok(());
    }
    if hits.is_empty() {
        println!("(no hits)");
        return Ok(());
    }
    for h in &hits {
        let hp = if h.heading_path.is_empty() {
            String::new()
        } else {
            format!(" § {}", h.heading_path.join(" / "))
        };
        println!("[{:.3}] {} / {}{}", h.score, h.source_id, h.external_id, hp);
        let preview: String = h.text.chars().take(180).collect();
        println!("       {}", preview);
    }
    Ok(())
}
