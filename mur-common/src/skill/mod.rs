//! MuR skill ecosystem — see `docs/superpowers/specs/2026-05-24-mur-skill-ecosystem-design.md`.

pub mod manifest;
pub mod parser;
pub mod scan;
pub mod types;
pub mod validate;

pub use manifest::*;
pub use parser::{
    ParseError, parse_canonical, parse_legacy_markdown, parse_markdown, serialize_canonical,
    serialize_markdown,
};
pub use types::*;
pub use validate::{ValidationError, validate};
