//! `mur source list|status|remove|test|weight|enable|disable` handlers.

use anyhow::{Result, bail};

pub(super) async fn list(json: bool, _verbose: bool) -> Result<()> {
    use crate::sources::instance::SourceInstanceStore;
    let store = SourceInstanceStore::default_store()?;
    let items = store.list()?;
    if json {
        let j = serde_json::to_string_pretty(&items)?;
        println!("{j}");
        return Ok(());
    }
    if items.is_empty() {
        println!("(no sources — use `mur source add obsidian --vault <path>`)");
        return Ok(());
    }
    println!(
        "{:<22} {:<10} {:<8} {:>7} {:>7} {:<24}",
        "ID", "TYPE", "STATUS", "DOCS", "WEIGHT", "LAST SYNC"
    );
    for inst in &items {
        let status_str = if !inst.enabled { "off" } else { "ok" };
        let last = inst
            .sync
            .last_sync_at
            .map(|t| t.to_rfc3339())
            .unwrap_or_else(|| "never".into());
        println!(
            "{:<22} {:<10} {:<8} {:>7} {:>7.2} {:<24}",
            inst.id, inst.type_name, status_str, inst.stats.doc_count, inst.weight, last
        );
        if _verbose {
            println!("    scope: {:?}", inst.scope);
            if let Some(err) = &inst.sync.last_error {
                println!("    last_error: {err}");
            }
        }
    }
    Ok(())
}

pub(super) async fn status(id: Option<&str>) -> Result<()> {
    use crate::sources::instance::SourceInstanceStore;
    let store = SourceInstanceStore::default_store()?;
    let items = match id {
        Some(i) => vec![store.load(i)?],
        None => store.list()?,
    };
    if items.is_empty() {
        println!("(no sources)");
        return Ok(());
    }
    for inst in items {
        println!("─── {} ({}) ───", inst.id, inst.type_name);
        println!("  enabled     : {}", inst.enabled);
        println!("  weight      : {:.2}", inst.weight);
        println!("  scope       : {:?}", inst.scope);
        println!(
            "  last_sync_at: {}",
            inst.sync
                .last_sync_at
                .map(|t| t.to_rfc3339())
                .unwrap_or_else(|| "never".into())
        );
        println!(
            "  last_cursor : {}",
            inst.sync.last_cursor.unwrap_or_else(|| "none".into())
        );
        println!("  docs        : {}", inst.stats.doc_count);
        println!("  chunks      : {}", inst.stats.chunk_count);
        if let Some(err) = &inst.sync.last_error {
            println!("  last_error  : {err}");
        }
        if !inst.sync.errors_tail.is_empty() {
            println!("  errors_tail : {} entries", inst.sync.errors_tail.len());
        }
    }
    Ok(())
}

pub(super) async fn remove(id: &str, keep_index: bool) -> Result<()> {
    use crate::sources::instance::SourceInstanceStore;
    use crate::store::vector::factory::get_vector_store;
    use anyhow::Context;

    let store = SourceInstanceStore::default_store()?;
    let _ = store
        .load(id)
        .with_context(|| format!("source `{id}` not found"))?;

    if !keep_index {
        let cfg = crate::store::config::load_config()?;
        let index_path = mur_common::home::mur_home_or_err()?.join("index");
        let vs = get_vector_store(&cfg, &index_path).await?;
        vs.delete_by_source(id)
            .await
            .context("delete source chunks")?;
        let tantivy = crate::sources::tantivy::TantivyIndex::open_or_create(
            &mur_common::home::mur_home_or_err()?,
        )?;
        tantivy
            .delete_by_source(id)
            .context("tantivy.delete_by_source")?;
        println!("🗑  removed indexed chunks for {id}");
    }
    store.delete(id)?;
    println!("🗑  removed yaml for {id}");
    Ok(())
}

pub(super) async fn test_source(id: &str) -> Result<()> {
    use crate::sources::KnowledgeSource;
    use crate::sources::adapters::obsidian::ObsidianAdapter;
    use crate::sources::instance::SourceInstanceStore;
    use crate::sources::types::DocumentBody;
    use crate::store::embedding::{EmbeddingConfig, embed};
    use std::time::Instant;

    let store = SourceInstanceStore::default_store()?;
    let inst = store.load(id)?;
    if inst.type_name != "obsidian" {
        bail!(
            "test only supports obsidian in P1.2; got `{}`",
            inst.type_name
        );
    }
    let adapter = ObsidianAdapter::from_instance(&inst)?;

    let t0 = Instant::now();
    let (docs, _cursor) = adapter.list_documents(None).await?;
    println!(
        "→ list_documents: {} docs in {:?}",
        docs.len(),
        t0.elapsed()
    );
    if docs.is_empty() {
        println!("   (no documents — nothing to test)");
        return Ok(());
    }
    let doc_ref = &docs[0];
    println!("→ sampling first doc: {}", doc_ref.external_id);
    let t0 = Instant::now();
    let doc = adapter.fetch(doc_ref).await?;
    let body_len = match &doc.body {
        DocumentBody::Markdown(s) | DocumentBody::PlainText(s) => s.len(),
        DocumentBody::NotionBlocks(_) => 0,
    };
    println!("  fetch: {} chars in {:?}", body_len, t0.elapsed());

    let t0 = Instant::now();
    let chunks = adapter.chunk(&doc)?;
    println!("  chunk: {} chunks in {:?}", chunks.len(), t0.elapsed());

    let cfg = crate::store::config::load_config()?;
    let emb_cfg = EmbeddingConfig::from_config(&cfg);
    let sample = chunks.first().map(|c| c.text.clone()).unwrap_or_default();
    let t0 = Instant::now();
    let v = embed(&sample, &emb_cfg).await?;
    println!("  embed: {} dims in {:?}", v.len(), t0.elapsed());

    println!("✅ adapter working");
    Ok(())
}

// ---------- Task 14 handlers ----------

pub(super) async fn set_weight(id: &str, value: f32) -> Result<()> {
    use crate::sources::instance::SourceInstanceStore;
    if !(0.0..=2.0).contains(&value) {
        bail!("weight must be in [0.0, 2.0], got {value}");
    }
    let store = SourceInstanceStore::default_store()?;
    let mut inst = store.load(id)?;
    inst.weight = value;
    store.save(&inst)?;
    println!("✏️  {id} weight set to {value:.2}");
    Ok(())
}

pub(super) async fn set_enabled(id: &str, enabled: bool) -> Result<()> {
    use crate::sources::instance::SourceInstanceStore;
    let store = SourceInstanceStore::default_store()?;
    let mut inst = store.load(id)?;
    inst.enabled = enabled;
    store.save(&inst)?;
    println!("✏️  {id} {}", if enabled { "enabled" } else { "disabled" });
    Ok(())
}
