//! Clap dispatch for `mur agent companion card …`.
//!
//! WHY a dedicated module: keeps the clap argument types out of the
//! `accept`/`import`/`list`/`export` library modules so each library entry
//! point stays callable from tests and from future programmatic callers
//! (e.g. the Tauri admin facade) without dragging clap into the public
//! surface.

use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct CardArgs {
    #[command(subcommand)]
    pub cmd: CardCmd,
}

#[derive(Subcommand, Debug)]
pub enum CardCmd {
    /// Export the agent profile as a .murcard.yaml.
    Export {
        /// Agent name.
        name: String,
        /// Output file (defaults to <agent_home>/<name>.murcard.yaml).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Sign the exported card with the agent's identity.key.
        #[arg(long)]
        sign: bool,
    },
    /// Import a card from a PNG, .murcard.yaml, or c.ai JSON file.
    /// The card lands in the inbox; run `card accept` to apply it.
    Import {
        /// Agent name.
        name: String,
        /// Path to source file (auto-detected by content).
        source: PathBuf,
    },
    /// List pending cards in the inbox for an agent.
    List {
        /// Agent name.
        name: String,
        /// Emit JSON instead of a human-readable table.
        #[arg(long)]
        json: bool,
    },
    /// Accept (apply) one inbox card by id.
    Accept {
        /// Agent name.
        name: String,
        /// Card id (ULID from `card list`).
        id: String,
    },
}

pub async fn run(args: CardArgs) -> anyhow::Result<()> {
    match args.cmd {
        CardCmd::Export { name, out, sign } => {
            let p = super::export::export_card(&name, out.as_deref(), sign).await?;
            println!("exported {}", p.display());
            Ok(())
        }
        CardCmd::Import { name, source } => {
            let res = super::import::import_card(&name, &source).await?;
            println!(
                "imported id={} trust={} -> {}",
                res.id,
                res.trust,
                res.path.display()
            );
            Ok(())
        }
        CardCmd::List { name, json } => {
            let entries = super::list::list_inbox(&name)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&entries)?);
            } else if entries.is_empty() {
                println!("(inbox is empty)");
            } else {
                println!("{:<28}  {:<30}  {:<10}  imported_at", "id", "name", "trust");
                for e in entries {
                    println!(
                        "{:<28}  {:<30}  {:<10}  {}",
                        e.id, e.name, e.trust, e.imported_at
                    );
                }
            }
            Ok(())
        }
        CardCmd::Accept { name, id } => {
            super::accept::accept_card(&name, &id).await?;
            println!("accepted card {id}");
            Ok(())
        }
    }
}
