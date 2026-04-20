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
        SourceCommand::Add { .. } => {
            bail!("`mur source add` arrives in P1.2 (obsidian) / P1.4 (notion, joplin)")
        }
        SourceCommand::List { .. } => bail!("`mur source list` arrives in P1.2"),
        SourceCommand::Remove { .. } => bail!("`mur source remove` arrives in P1.2"),
        SourceCommand::Sync { .. } => bail!("`mur source sync` arrives in P1.2"),
        SourceCommand::Status { .. } => bail!("`mur source status` arrives in P1.2"),
        SourceCommand::Weight { .. } => bail!("`mur source weight` arrives in P1.2"),
        SourceCommand::Test { .. } => bail!("`mur source test` arrives in P1.2"),
        SourceCommand::Reindex { .. } => bail!("`mur source reindex` arrives in P1.3"),
        SourceCommand::InstallSchedule => bail!("`mur source install-schedule` arrives in P1.4"),
        SourceCommand::Disable { .. } => bail!("`mur source disable` arrives in P1.2"),
        SourceCommand::Enable { .. } => bail!("`mur source enable` arrives in P1.2"),
    }
}
