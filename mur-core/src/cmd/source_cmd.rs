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
            AddKind::Notion { .. } => bail!("`mur source add notion` arrives in P1.4"),
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
        SourceCommand::Reindex { .. } => bail!("`mur source reindex` arrives in P1.3"),
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

// ---------- stubs until Task 13/14 ----------

async fn sync(_id: Option<&str>, _full: bool) -> Result<()> {
    bail!("`mur source sync` arrives in Task 13")
}

async fn remove(_id: &str, _keep_index: bool) -> Result<()> {
    bail!("`mur source remove` arrives in Task 13")
}

async fn test_source(_id: &str) -> Result<()> {
    bail!("`mur source test` arrives in Task 13")
}

async fn set_weight(_id: &str, _value: f32) -> Result<()> {
    bail!("`mur source weight` arrives in Task 14")
}

async fn set_enabled(_id: &str, _enabled: bool) -> Result<()> {
    bail!("`mur source enable/disable` arrives in Task 14")
}
