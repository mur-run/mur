//! Text chunking utilities shared across adapters.
//!
//! `markdown::chunk_markdown` is used by the Obsidian and (later) Joplin
//! adapters. Notion uses a block-aware chunker that will live as a sibling
//! module in P1.4 (`notion_blocks.rs`).

pub mod markdown;
