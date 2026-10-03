//! `mur source ...` subcommand tree.
//!
//! P1.1 wires up the tree with every verb returning a "not yet implemented"
//! error, gated behind the `sources` feature flag. P1.2–P1.4 fill in each verb.

mod add;
mod manage;
mod reindex;
mod sync;

use add::{add_joplin, add_notion, add_obsidian};
use anyhow::Result;
use clap::Subcommand;
use manage::{list, remove, set_enabled, set_weight, status, test_source};
use reindex::{reindex, search};
use sync::{install_schedule, sync, sync_watch};

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
    let id = match &cmd {
        SourceCommand::Sync { id, .. } => id.clone(),
        SourceCommand::Reindex { id, .. } | SourceCommand::Remove { id, .. } => Some(id.clone()),
        _ => None,
    };
    handle_inner(cmd).await.map_err(|e| {
        let (what, command) = crate::store::vector::unreadable::hint::sources(id.as_deref());
        crate::store::vector::unreadable::with_rebuild_hint(e, what, &command)
    })
}

async fn handle_inner(cmd: SourceCommand) -> Result<()> {
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
            AddKind::Joplin {
                instance,
                db,
                server,
                token,
            } => add_joplin(instance, db, server, token).await,
        },
        SourceCommand::List { json, verbose } => list(json, verbose).await,
        SourceCommand::Remove { id, keep_index } => remove(&id, keep_index).await,
        SourceCommand::Sync { id, full, watch } => {
            if watch {
                sync_watch().await
            } else {
                sync(id.as_deref(), full).await
            }
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
        SourceCommand::InstallSchedule => install_schedule().await,
        SourceCommand::Disable { id } => set_enabled(&id, false).await,
        SourceCommand::Enable { id } => set_enabled(&id, true).await,
    }
}
