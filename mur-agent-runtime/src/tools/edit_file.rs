//! First-party exact-literal edit tool (issue #591, PR2). No regex —
//! ambiguity fails closed instead of guessing.

use mur_common::agent::FilesystemEntitlement;

use crate::llm::ToolDef;
use crate::tools::fs_policy::{SessionCwd, check_write_entitlement};
use crate::tools::{ToolError, ToolExecutor, ToolOutput};

mod apply;
mod hint;
use apply::{apply_edits, parse_edits};

/// The tool's registered name. B0 (#2181) and the registry key off this.
pub const EDIT_FILE: &str = "edit_file";

pub struct EditFileTool {
    pub session_cwd: SessionCwd,
    pub fs: FilesystemEntitlement,
    /// MUR's own launch chain. Checked before `fs`; no grant can satisfy it.
    pub chain: crate::sandbox::launch_chain::LaunchChain,
    /// This agent's canonical name, so a refusal prints a runnable grant.
    pub agent: String,
}

impl EditFileTool {
    pub fn new(
        session_cwd: SessionCwd,
        fs: FilesystemEntitlement,
        chain: crate::sandbox::launch_chain::LaunchChain,
        agent: String,
    ) -> Self {
        Self {
            session_cwd,
            fs,
            chain,
            agent,
        }
    }

    /// Test-only: construct with an inert launch chain. Production must go
    /// through `new`, which cannot be called without a real chain.
    #[cfg(test)]
    pub fn new_for_test(session_cwd: SessionCwd, fs: FilesystemEntitlement) -> Self {
        Self::new(
            session_cwd,
            fs,
            crate::sandbox::launch_chain::LaunchChain::inert(),
            "test-agent".into(),
        )
    }
}

#[async_trait::async_trait]
impl ToolExecutor for EditFileTool {
    fn name(&self) -> &str {
        EDIT_FILE
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: EDIT_FILE.into(),
            description: "Replace exact literal substrings in a UTF-8 text file (no regex). Either pass one edit as old_string/new_string, or several as `edits` (applied in order, each one seeing the result of the previous; all-or-nothing — if any edit fails nothing is written). Each old_string must match exactly once unless that edit sets expected_count to replace N known occurrences. Relative paths resolve against the shared session cwd (set by the bash tool's `cwd`); a `cd` inside a bash subprocess is NOT retained. Edits are checked against the agent's filesystem write entitlements."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": format!("File to edit ({})", crate::tools::fs_policy::PATH_FORMS) },
                    "old_string": { "type": "string", "description": "Exact literal text to replace (single-edit form)" },
                    "new_string": { "type": "string", "description": "Replacement text (single-edit form)" },
                    "expected_count": { "type": "integer", "description": "Exact number of occurrences to replace; omit to require exactly one (single-edit form)" },
                    "edits": {
                        "type": "array",
                        "description": "Several edits to the same file, applied in order. Use instead of old_string/new_string, not together with them.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "old_string": { "type": "string", "description": "Exact literal text to replace" },
                                "new_string": { "type": "string", "description": "Replacement text" },
                                "expected_count": { "type": "integer", "description": "Exact number of occurrences to replace; omit to require exactly one" }
                            },
                            "required": ["old_string", "new_string"]
                        }
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let raw = input["path"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidInput("missing 'path' field".into()))?;
        let edits = parse_edits(&input)?;
        let base = self.session_cwd.current();
        let joined = crate::tools::fs_policy::resolve_path(&base, raw);
        let canonical = std::fs::canonicalize(&joined).map_err(|e| {
            ToolError::Execution(crate::tools::fs_policy::format_io_error(
                "edit", &joined, &base, &e,
            ))
        })?;
        check_write_entitlement(&self.agent, &self.fs, &canonical, &self.chain)?;

        let text = std::fs::read_to_string(&canonical).map_err(|e| {
            ToolError::Execution(crate::tools::fs_policy::format_io_error(
                "read", &canonical, &base, &e,
            ))
        })?;
        let (updated, total) = apply_edits(&text, &edits)?;
        crate::tools::atomic_write::replace_contents(&canonical, updated.as_bytes()).map_err(
            |e| {
                ToolError::Execution(crate::tools::fs_policy::format_io_error(
                    "write", &canonical, &base, &e,
                ))
            },
        )?;
        let summary = if edits.len() == 1 {
            format!("replaced {total} occurrence(s) in {}", canonical.display())
        } else {
            format!(
                "applied {} edits ({total} occurrence(s)) in {}",
                edits.len(),
                canonical.display()
            )
        };
        Ok(summary.into())
    }
}

#[cfg(test)]
mod tests;
