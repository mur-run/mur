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

pub use capability::{allowed_for, check_capabilities, Capability, CapabilityViolation};
pub use hash::{content_sha256, ct_eq_hex, drift_status, sha256_hex, DriftStatus};
pub use manifest::*;
pub use parser::{
    parse_canonical, parse_legacy_markdown, parse_markdown, serialize_canonical,
    serialize_markdown, yaml_to_markdown, ParseError,
};
pub use sign::{sign_manifest, verify_manifest, SignError, SKILL_PAYLOAD_TYPE};
pub use store::{agent_skill_dir, global_skill_dir, read_from_dir, write_to_dir, StoreError};
pub use types::*;
pub use validate::{validate, ValidationError};
