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
    },

    /// Rank existing notes by a keyword query.
    Search {
        /// The search query.
        query: String,

        /// Maximum results to print.
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
}
