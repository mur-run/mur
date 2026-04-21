//! `mur source sync --watch` orchestrator.
//!
//! Combines:
//!   - `notify` file-watcher events for local-file adapters (Obsidian, Joplin local)
//!   - `tokio::time::interval` polling for cloud adapters (Notion, Joplin Server)
//!
//! Foreground daemon — Ctrl+C exits cleanly.

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::sources::instance::{SourceInstance, SourceInstanceStore};
use crate::sources::tantivy::TantivyIndex;
use crate::store::embedding::EmbeddingConfig;
use crate::store::vector::VectorStore;

pub struct WatchOptions {
    pub poll_interval_secs: u64,
}

/// Run watch mode. Loops forever (until SIGINT). Logs each sync via tracing.
pub async fn run_watch(
    instance_store: SourceInstanceStore,
    vector_store: Arc<dyn VectorStore>,
    tantivy: TantivyIndex,
    embedding_cfg: EmbeddingConfig,
    opts: WatchOptions,
) -> Result<()> {
    let instances = instance_store.list()?;
    if instances.is_empty() {
        println!("(no sources to watch)");
        return Ok(());
    }
    println!(
        "watching {} source(s); poll interval = {}s; Ctrl+C to stop",
        instances.len(),
        opts.poll_interval_secs
    );

    let (tx, mut rx) = mpsc::unbounded_channel::<String>(); // source_id needing sync

    // Spawn a debouncer for each Obsidian vault.
    let mut watcher_handles: Vec<notify::RecommendedWatcher> = Vec::new();
    for inst in &instances {
        if inst.type_name == "obsidian" && inst.enabled {
            if let Some(vault) = inst.scope.get("vault").and_then(|v| v.as_str()) {
                let vp = PathBuf::from(vault);
                let id = inst.id.clone();
                let tx_clone = tx.clone();
                use notify::{Event, RecursiveMode, Watcher};
                let mut w = notify::recommended_watcher(move |res: notify::Result<Event>| {
                    if let Ok(ev) = res {
                        let touches_md = ev
                            .paths
                            .iter()
                            .any(|p| p.extension().is_some_and(|e| e == "md"));
                        if touches_md {
                            let _ = tx_clone.send(id.clone());
                        }
                    }
                })
                .context("create file watcher")?;
                w.watch(&vp, RecursiveMode::Recursive)
                    .with_context(|| format!("watch {}", vp.display()))?;
                watcher_handles.push(w);
            }
        }
    }

    // Cloud-poll ticker.
    let mut poll = tokio::time::interval(Duration::from_secs(opts.poll_interval_secs));
    poll.tick().await; // skip first immediate tick

    let cloud_ids: Vec<String> = instances
        .iter()
        .filter(|i| {
            i.enabled
                && (i.type_name == "notion"
                    || (i.type_name == "joplin"
                        && i.scope.get("server_url").is_some()))
        })
        .map(|i| i.id.clone())
        .collect();

    // Debouncer: collapse rapid file events on the same source within 500ms.
    let mut last_sent: std::collections::HashMap<String, std::time::Instant> =
        std::collections::HashMap::new();

    loop {
        tokio::select! {
            biased;
            _ = tokio::signal::ctrl_c() => {
                println!("\nreceived Ctrl+C, exiting");
                break;
            }
            _ = poll.tick() => {
                for id in &cloud_ids {
                    let _ = tx.send(id.clone());
                }
            }
            Some(src_id) = rx.recv() => {
                let now = std::time::Instant::now();
                if let Some(prev) = last_sent.get(&src_id) {
                    if now.duration_since(*prev) < Duration::from_millis(500) {
                        continue;
                    }
                }
                last_sent.insert(src_id.clone(), now);
                tracing::info!(source = %src_id, "watch: triggering sync");
                if let Err(e) = sync_one(
                    &src_id,
                    &instance_store,
                    vector_store.clone(),
                    &tantivy,
                    &embedding_cfg,
                )
                .await
                {
                    tracing::warn!(source = %src_id, error = %e, "watch: sync failed");
                }
            }
        }
    }
    drop(watcher_handles);
    Ok(())
}

async fn sync_one(
    source_id: &str,
    instance_store: &SourceInstanceStore,
    vector_store: Arc<dyn VectorStore>,
    tantivy: &TantivyIndex,
    embedding_cfg: &EmbeddingConfig,
) -> Result<()> {
    use crate::sources::adapters::obsidian::ObsidianAdapter;
    use crate::sources::sync::sync_source;

    let mut inst: SourceInstance = instance_store.load(source_id)?;
    // Watch mode is incremental — `full = false` so we don't re-embed everything per file change.
    match inst.type_name.as_str() {
        "obsidian" => {
            let adapter = ObsidianAdapter::from_instance(&inst)?;
            sync_source(
                &adapter,
                &mut inst,
                instance_store,
                vector_store,
                tantivy,
                embedding_cfg,
                false,
            )
            .await?;
        }
        "notion" => {
            use crate::sources::adapters::notion::NotionAdapter;
            use crate::sources::credentials::{CredentialStore, OsKeyring, SERVICE};
            let kr = OsKeyring;
            let kr_account = inst
                .keyring_entry
                .clone()
                .unwrap_or_else(|| format!("{}:access_token", inst.id));
            let token = kr
                .get(SERVICE, &kr_account)?
                .ok_or_else(|| anyhow::anyhow!("no notion token in keyring for `{}`", inst.id))?;
            let adapter = NotionAdapter::from_instance(&inst, token)?;
            sync_source(
                &adapter,
                &mut inst,
                instance_store,
                vector_store,
                tantivy,
                embedding_cfg,
                false,
            )
            .await?;
        }
        "joplin" => {
            use crate::sources::adapters::joplin::JoplinAdapter;
            use crate::sources::credentials::{CredentialStore, OsKeyring, SERVICE};
            let token = if inst.scope.get("server_url").is_some() {
                let kr = OsKeyring;
                let kr_account = inst
                    .keyring_entry
                    .clone()
                    .unwrap_or_else(|| format!("{}:api_token", inst.id));
                Some(
                    kr.get(SERVICE, &kr_account)?
                        .ok_or_else(|| anyhow::anyhow!("no joplin token"))?,
                )
            } else {
                None
            };
            let adapter = JoplinAdapter::from_instance(&inst, token)?;
            sync_source(
                &adapter,
                &mut inst,
                instance_store,
                vector_store,
                tantivy,
                embedding_cfg,
                false,
            )
            .await?;
        }
        other => {
            anyhow::bail!("watch: unsupported adapter type `{other}`");
        }
    }
    Ok(())
}
