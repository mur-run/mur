//! MuR skill ecosystem — see `docs/superpowers/specs/2026-05-24-mur-skill-ecosystem-design.md`.

pub mod aggregator;
pub mod capability;
pub mod constraint;
pub mod evolution;
pub mod hash;
pub mod inventory;
pub mod lifecycle;
pub mod loader;
pub mod local;
pub mod lockfile;
pub mod manifest;
pub mod mcp;
pub mod parser;
pub mod registry;
pub mod resolve;
pub mod scan;
pub mod sign;
pub mod stats;
pub mod store;
pub mod types;
pub mod validate;
pub mod version;

pub use capability::{Capability, CapabilityViolation, allowed_for, check_capabilities};
pub use constraint::{Constraint, ConstraintError};
pub use evolution::EvolutionEvent;
pub use hash::{
    DriftStatus, content_hash_for_trust, content_sha256, ct_eq_hex, drift_status, sha256_hex,
};
pub use inventory::McpInventory;
pub use lifecycle::{
    calculate_decay, half_life_days, next_state, on_promotion, transition_allowed,
};
pub use loader::{LoadedSkill, SkillScope, load_all};
pub use lockfile::{LockfileError, SkillLock};
pub use manifest::*;
pub use mcp::{McpRequirement, ParseCapabilityError, SkillCapability, validate_requirements};
pub use parser::{
    ParseError, parse_canonical, parse_legacy_markdown, parse_markdown, serialize_canonical,
    serialize_markdown, yaml_to_markdown,
};
pub use resolve::{Resolution, resolve_step};
pub use sign::{SKILL_PAYLOAD_TYPE, SignError, sign_manifest, verify_manifest};
pub use stats::{LifecycleState, SkillStats};
pub use store::{StoreError, agent_skill_dir, global_skill_dir, read_from_dir, write_to_dir};
pub use types::*;
pub use validate::{ValidationError, validate};
pub use version::{SKILL_MANIFEST_SCHEMA_VERSION, is_supported};
