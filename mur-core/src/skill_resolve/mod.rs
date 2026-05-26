//! Re-exported from `mur-common::skill` so `mur-core` callers have a single
//! import path. The resolver itself lives in `mur-common` because
//! `mur-agent-runtime` also needs it and does not depend on `mur-core`.

pub use mur_common::skill::McpInventory;
pub use mur_common::skill::{Resolution, resolve_step};

pub mod inventory {
    //! Re-export for callers that import `mur_core::skill_resolve::inventory::McpInventory`.
    pub use mur_common::skill::McpInventory;
}
