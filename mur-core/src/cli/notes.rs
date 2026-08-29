//! `mur notes` CLI subcommands — MVP: create + search.

use std::path::PathBuf;

use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum NotesAction {
    /// Create a new note. Body comes from --body-file or stdin.
    Create {
        /// Unique kebab-case name (lowercase letters, digits, hyphens; ≤ 64 chars).
        name: String,

        /// One-line description (also stored as content.abstract).
        #[arg(long, short = 'd')]
        description: String,

        /// Read the markdown body from this file. If omitted, body is read from stdin.
        #[arg(long)]
        body_file: Option<PathBuf>,

        /// Note kind: `rule` (behavioral guidance, fast decay) or `fact`
        /// (environment truth, slow decay).
        #[arg(long, default_value = "fact")]
        kind: String,
    },

    /// Rank existing notes by a keyword query.
    Search {
        /// The search query.
        query: String,

        /// Maximum results to print.
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },

    /// Remove a note. Global by default; `--agent` targets that agent's own
    /// memory instead.
    Remove {
        /// Note name.
        name: String,

        /// Remove this agent's own memory rather than a global note.
        #[arg(long)]
        agent: Option<String>,
    },

    /// List notes, optionally filtered by maturity.
    List {
        /// Filter by lifecycle maturity: draft|emerging|stable|canonical|deprecated|archived.
        #[arg(long)]
        maturity: Option<String>,

        /// Maximum notes to print.
        #[arg(long, default_value_t = 50)]
        limit: usize,

        /// Also show this agent's own memories, labelled by scope. Without it
        /// the listing covers global notes only — an agent's own memories live
        /// under its home and are invisible here.
        #[arg(long)]
        agent: Option<String>,
    },

    /// Print a single note's body and maturity (records a retrieval).
    Show {
        /// The note name.
        name: String,
    },
}
