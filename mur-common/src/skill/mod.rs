//! MuR skill ecosystem — see `docs/superpowers/specs/2026-05-24-mur-skill-ecosystem-design.md`.

pub mod manifest;
pub mod parser;
pub mod types;

pub use manifest::*;
pub use parser::{parse_canonical, serialize_canonical, ParseError};
pub use types::*;
