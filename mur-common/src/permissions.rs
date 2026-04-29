//! Permission grants and audit log for the AskUser flow.
//!
//! M0.1: stub (`ScopeKey` only) so the hook trait surface compiles.
//! M0.2.2 expands to full `Grant` / `GrantStore` / `AuditEvent`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ScopeKey {
    pub agent_id: String,
    pub tool_name: String,
    /// SHA-256 (hex) over the canonical-JSON of a per-tool subset of inputs.
    /// Each tool declares which input fields contribute (e.g. bash → argv[0];
    /// fs.write → directory prefix).
    pub input_schema_hash: String,
}
