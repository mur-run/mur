//! `mur source ...` subcommand tree.
//!
//! P1.1 wires up the tree with every verb returning a "not yet implemented"
//! error, gated behind the `sources` feature flag. P1.2–P1.4 fill in each verb.

use anyhow::{Result, bail};
use clap::Subcommand;

#[derive(Subcommand)]
pub enum SourceCommand {
    /// Register a new source.
    Add {
        #[command(subcommand)]
        kind: AddKind,
    },
    /// List registered sources.
    List {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        verbose: bool,
    },
    /// Remove a source (credentials + index).
    Remove {
        id: String,
        #[arg(long)]
        keep_index: bool,
    },
    /// Sync one or all sources.
    Sync {
        id: Option<String>,
        #[arg(long)]
        full: bool,
        #[arg(long)]
        watch: bool,
    },
    /// Show sync health for a source.
    Status {
        id: Option<String>,
    },
    /// Set the retrieve weight.
    Weight {
        id: String,
        value: f32,
    },
    /// Dry-run a single document through the adapter.
    Test {
        id: String,
    },
    /// Rebuild the vector index for a source.
    Reindex {
        id: String,
        #[arg(long)]
        vector_backend: Option<String>,
    },
    /// Search indexed source chunks (minimal, sources-only — for P1.3 see `mur search`).
    Search {
        query: String,
        #[arg(long, short = 'k', default_value_t = 5)]
        limit: usize,
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Generate launchd / systemd unit files for scheduled sync.
    InstallSchedule,
    Disable {
        id: String,
    },
    Enable {
        id: String,
    },
}

#[derive(Subcommand)]
pub enum AddKind {
    /// Connect a Notion workspace (OAuth or Integration Token).
    Notion {
        instance: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long)]
        token: Option<String>,
    },
    /// Connect an Obsidian vault (local markdown folder).
    Obsidian {
        instance: Option<String>,
        #[arg(long)]
        vault: std::path::PathBuf,
        #[arg(long, value_delimiter = ',')]
        exclude_folder: Vec<String>,
    },
    /// Connect Joplin (local SQLite or Joplin Server).
    Joplin {
        instance: Option<String>,
        #[arg(long, conflicts_with = "server")]
        db: Option<std::path::PathBuf>,
        #[arg(long, requires = "token")]
        server: Option<String>,
        #[arg(long, requires = "server")]
        token: Option<String>,
    },
}

pub async fn handle(cmd: SourceCommand) -> Result<()> {
    match cmd {
        SourceCommand::Add { kind } => match kind {
            AddKind::Obsidian {
                instance,
                vault,
                exclude_folder,
            } => add_obsidian(instance, vault, exclude_folder).await,
            AddKind::Notion {
                instance,
                workspace,
                token,
            } => add_notion(instance, workspace, token).await,
            AddKind::Joplin { .. } => bail!("`mur source add joplin` arrives in P1.4"),
        },
        SourceCommand::List { json, verbose } => list(json, verbose).await,
        SourceCommand::Remove { id, keep_index } => remove(&id, keep_index).await,
        SourceCommand::Sync { id, full, watch } => {
            if watch {
                bail!("`mur source sync --watch` arrives in P1.4");
            }
            sync(id.as_deref(), full).await
        }
        SourceCommand::Status { id } => status(id.as_deref()).await,
        SourceCommand::Weight { id, value } => set_weight(&id, value).await,
        SourceCommand::Test { id } => test_source(&id).await,
        SourceCommand::Search {
            query,
            limit,
            source,
            json,
        } => search(&query, limit, source.as_deref(), json).await,
        SourceCommand::Reindex { id, vector_backend } => {
            reindex(&id, vector_backend.as_deref()).await
        }
        SourceCommand::InstallSchedule => bail!("`mur source install-schedule` arrives in P1.4"),
        SourceCommand::Disable { id } => set_enabled(&id, false).await,
        SourceCommand::Enable { id } => set_enabled(&id, true).await,
    }
}

// ---------- Task 12 handlers ----------

async fn add_obsidian(
    instance: Option<String>,
    vault: std::path::PathBuf,
    exclude_folder: Vec<String>,
) -> Result<()> {
    use crate::sources::instance::{SourceInstance, SourceInstanceStore, SourceStats, SyncState};
    use crate::sources::kind::SourceKind;
    use anyhow::Context;
    use std::collections::BTreeMap;

    let store = SourceInstanceStore::default_store()?;
    let id = match instance {
        Some(tag) if !tag.is_empty() => format!("obsidian:{tag}"),
        _ => {
            let existing: Vec<String> = store.list()?.into_iter().map(|i| i.id).collect();
            if !existing.iter().any(|id| id == "obsidian") {
                "obsidian".to_string()
            } else {
                let mut rng: u16 = rand::random();
                loop {
                    let candidate = format!("obsidian:{rng:04x}");
                    if !existing.contains(&candidate) {
                        break candidate;
                    }
                    rng = rng.wrapping_add(1);
                }
            }
        }
    };

    let abs_vault = std::fs::canonicalize(&vault)
        .with_context(|| format!("resolve vault path {}", vault.display()))?;
    if !abs_vault.is_dir() {
        bail!("vault path is not a directory: {}", abs_vault.display());
    }

    let mut scope: BTreeMap<String, serde_yaml::Value> = BTreeMap::new();
    scope.insert(
        "vault".into(),
        serde_yaml::Value::String(abs_vault.to_string_lossy().to_string()),
    );
    if !exclude_folder.is_empty() {
        scope.insert(
            "exclude_folders".into(),
            serde_yaml::Value::Sequence(
                exclude_folder
                    .into_iter()
                    .map(serde_yaml::Value::String)
                    .collect(),
            ),
        );
    }

    let inst = SourceInstance {
        id: id.clone(),
        type_name: "obsidian".into(),
        kind: SourceKind::PullIndex,
        enabled: true,
        weight: 1.0,
        scope,
        sync: SyncState::default(),
        stats: SourceStats::default(),
        keyring_entry: None,
    };
    store.save(&inst)?;
    println!("✅ Connected vault {} as `{}`", abs_vault.display(), id);
    println!("Run `mur source sync {id}` to index.");
    Ok(())
}

async fn add_notion(
    instance: Option<String>,
    workspace: Option<String>,
    token: Option<String>,
) -> Result<()> {
    use crate::sources::adapters::notion::{OAuthResult, run_oauth_flow};
    use crate::sources::credentials::{CredentialStore, OsKeyring, SERVICE, account};
    use crate::sources::instance::{SourceInstance, SourceInstanceStore, SourceStats, SyncState};
    use crate::sources::kind::SourceKind;
    use anyhow::Context;
    use std::collections::BTreeMap;

    let store = SourceInstanceStore::default_store()?;
    let id = match instance {
        Some(tag) if !tag.is_empty() => format!("notion:{tag}"),
        _ => {
            let existing: Vec<String> = store.list()?.into_iter().map(|i| i.id).collect();
            if !existing.iter().any(|s| s == "notion") {
                "notion".to_string()
            } else {
                let mut rng: u16 = rand::random();
                loop {
                    let candidate = format!("notion:{rng:04x}");
                    if !existing.contains(&candidate) {
                        break candidate;
                    }
                    rng = rng.wrapping_add(1);
                }
            }
        }
    };

    let (access_token, workspace_id, workspace_name) = if let Some(pat) = token {
        (pat, workspace, None::<String>)
    } else {
        println!("-> launching Notion OAuth (PKCE) flow...");
        let OAuthResult {
            access_token,
            workspace_id,
            workspace_name,
        } = run_oauth_flow().await?;
        (access_token, workspace_id, workspace_name)
    };

    // Persist credentials to keyring
    let keyring = OsKeyring;
    let kr_account = account(&id, "access_token");
    keyring
        .set(SERVICE, &kr_account, &access_token)
        .context("store notion access_token in keyring")?;

    let mut scope: BTreeMap<String, serde_yaml::Value> = BTreeMap::new();
    if let Some(w) = workspace_id {
        scope.insert("workspace_id".into(), serde_yaml::Value::String(w));
    }
    if let Some(n) = workspace_name {
        scope.insert("workspace_name".into(), serde_yaml::Value::String(n));
    }

    let inst = SourceInstance {
        id: id.clone(),
        type_name: "notion".into(),
        kind: SourceKind::PullIndex,
        enabled: true,
        weight: 1.0,
        scope,
        sync: SyncState::default(),
        stats: SourceStats::default(),
        keyring_entry: Some(kr_account.clone()),
    };
    store.save(&inst)?;
    println!("Connected Notion as `{id}`");
    println!("Run `mur source sync {id}` to index.");
    Ok(())
}

async fn list(json: bool, _verbose: bool) -> Result<()> {
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

async fn status(id: Option<&str>) -> Result<()> {
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

// ---------- Task 13 handlers ----------

async fn sync(id: Option<&str>, full: bool) -> Result<()> {
    use crate::sources::adapters::obsidian::ObsidianAdapter;
    use crate::sources::instance::SourceInstanceStore;
    use crate::sources::sync::sync_source;
    use crate::store::embedding::EmbeddingConfig;
    use crate::store::vector::factory::get_vector_store;
    use anyhow::Context;

    let cfg = crate::store::config::load_config()?;
    let emb_cfg = EmbeddingConfig::from_config(&cfg);
    let index_path = dirs::home_dir()
        .context("no home dir")?
        .join(".mur")
        .join("index");
    let vector_store = get_vector_store(&cfg, &index_path).await?;
    let tantivy = crate::sources::tantivy::TantivyIndex::open_or_create(
        &dirs::home_dir().context("no home dir")?.join(".mur"),
    )?;

    let store = SourceInstanceStore::default_store()?;
    let targets: Vec<crate::sources::instance::SourceInstance> = match id {
        Some(i) => vec![store.load(i)?],
        None => store
            .list()?
            .into_iter()
            .filter(|inst| inst.enabled)
            .collect(),
    };
    if targets.is_empty() {
        println!("(no enabled sources to sync)");
        return Ok(());
    }

    for mut inst in targets {
        if inst.type_name == "notion" {
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
            println!("↻ syncing {}{}", inst.id, if full { " (full)" } else { "" });
            let report = sync_source(
                &adapter,
                &mut inst,
                &store,
                vector_store.clone(),
                &tantivy,
                &emb_cfg,
                full,
            )
            .await?;
            println!(
                "  synced {} docs ({} chunks), deleted {}, {} errors",
                report.docs_synced,
                report.chunks_emitted,
                report.docs_deleted,
                report.errors.len()
            );
            for e in report.errors.iter().take(3) {
                println!("  ! {e}");
            }
            continue;
        }
        if inst.type_name != "obsidian" {
            println!(
                "⏭  {}: adapter `{}` arrives in a later sub-milestone",
                inst.id, inst.type_name
            );
            continue;
        }
        let adapter = ObsidianAdapter::from_instance(&inst)?;
        println!("↻ syncing {}{}", inst.id, if full { " (full)" } else { "" });
        let report = sync_source(
            &adapter,
            &mut inst,
            &store,
            vector_store.clone(),
            &tantivy,
            &emb_cfg,
            full,
        )
        .await?;
        println!(
            "  synced {} docs ({} chunks), deleted {}, {} errors",
            report.docs_synced,
            report.chunks_emitted,
            report.docs_deleted,
            report.errors.len()
        );
        for e in report.errors.iter().take(3) {
            println!("  ! {e}");
        }
    }
    Ok(())
}

async fn remove(id: &str, keep_index: bool) -> Result<()> {
    use crate::sources::instance::SourceInstanceStore;
    use crate::store::vector::factory::get_vector_store;
    use anyhow::Context;

    let store = SourceInstanceStore::default_store()?;
    let _ = store
        .load(id)
        .with_context(|| format!("source `{id}` not found"))?;

    if !keep_index {
        let cfg = crate::store::config::load_config()?;
        let index_path = dirs::home_dir()
            .context("no home dir")?
            .join(".mur")
            .join("index");
        let vs = get_vector_store(&cfg, &index_path).await?;
        vs.delete_by_source(id)
            .await
            .context("delete source chunks")?;
        let tantivy = crate::sources::tantivy::TantivyIndex::open_or_create(
            &dirs::home_dir().context("no home dir")?.join(".mur"),
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

async fn test_source(id: &str) -> Result<()> {
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

async fn set_weight(id: &str, value: f32) -> Result<()> {
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

async fn set_enabled(id: &str, enabled: bool) -> Result<()> {
    use crate::sources::instance::SourceInstanceStore;
    let store = SourceInstanceStore::default_store()?;
    let mut inst = store.load(id)?;
    inst.enabled = enabled;
    store.save(&inst)?;
    println!("✏️  {id} {}", if enabled { "enabled" } else { "disabled" });
    Ok(())
}

// ---------- Task 8 handlers ----------

async fn reindex(id: &str, vector_backend: Option<&str>) -> Result<()> {
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
    let vector_store = get_vector_store(&cfg, &index_path).await?;
    let tantivy =
        TantivyIndex::open_or_create(&dirs::home_dir().context("no home dir")?.join(".mur"))?;

    let store = SourceInstanceStore::default_store()?;
    let mut inst = store.load(id)?;

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
    Ok(())
}

// ---------- Task 15 handlers ----------

async fn search(query: &str, limit: usize, source: Option<&str>, json: bool) -> Result<()> {
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
