//! MuR skill ecosystem — see `docs/superpowers/specs/2026-05-24-mur-skill-ecosystem-design.md`.

pub mod capability;
pub mod hash;
pub mod manifest;
pub mod parser;
pub mod scan;
pub mod sign;
pub mod store;
pub mod types;
pub mod validate;

pub use capability::{Capability, CapabilityViolation, allowed_for, check_capabilities};
pub use hash::{DriftStatus, content_sha256, ct_eq_hex, drift_status, sha256_hex};
pub use manifest::*;
pub use parser::{
    ParseError, parse_canonical, parse_legacy_markdown, parse_markdown, serialize_canonical,
    serialize_markdown,
};
pub use sign::{sign_manifest, verify_manifest, SignError, SKILL_PAYLOAD_TYPE};
pub use types::*;
pub use store::{StoreError, agent_skill_dir, global_skill_dir, read_from_dir, write_to_dir};
pub use validate::{ValidationError, validate};
