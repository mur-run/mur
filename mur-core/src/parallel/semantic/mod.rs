#![allow(dead_code, unused_imports)]
//! Semantic unit extraction and content-addressable storage.

pub mod cas;
pub mod tree_sitter_parse;

use serde::{Deserialize, Serialize};
use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitKind {
    Fn,
    Struct,
    Impl,
    Trait,
    Enum,
    Const,
    Test,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticUnit {
    pub kind: UnitKind,
    pub name: String,
    pub byte_range: Range<usize>,
    pub line_range: Range<u32>,
    /// blake3 hash of the source bytes in this range.
    pub content_hash: [u8; 32],
    /// Names of other top-level units this unit references.
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportedLanguage {
    Rust,
}

pub use tree_sitter_parse::extract_units;
