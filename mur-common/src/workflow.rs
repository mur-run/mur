//! Workflow — a reusable sequence of steps captured from sessions.
//!
//! Workflows embed `KnowledgeBase` via `#[serde(flatten)]` so YAML stays flat.

use serde::{Deserialize, Serialize};

use crate::knowledge::KnowledgeBase;

/// A MUR workflow — a captured, reusable sequence of steps.
///
/// YAML files in `~/.mur/workflows/` are the source of truth.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workflow {
    /// Shared knowledge fields (flattened into YAML)
    #[serde(flatten)]
    pub base: KnowledgeBase,

    /// Ordered steps in this workflow
    #[serde(default)]
    pub steps: Vec<Step>,

    /// Variables/parameters for this workflow
    #[serde(default)]
    pub variables: Vec<Variable>,

    /// Session IDs this workflow was extracted from
    #[serde(default)]
    pub source_sessions: Vec<String>,

    /// Natural-language trigger description (e.g. "when deploying to production")
    #[serde(default)]
    pub trigger: String,

    /// Tools this workflow uses (e.g. ["cargo", "docker"])
    #[serde(default)]
    pub tools: Vec<String>,

    /// Published version number (incremented on each publish)
    #[serde(default)]
    pub published_version: u32,

    /// Permission level required to run this workflow
    #[serde(default)]
    pub permission: Permission,
}

// Allow `workflow.name`, `workflow.content`, etc. via auto-deref.
impl std::ops::Deref for Workflow {
    type Target = KnowledgeBase;
    fn deref(&self) -> &KnowledgeBase {
        &self.base
    }
}
impl std::ops::DerefMut for Workflow {
    fn deref_mut(&mut self) -> &mut KnowledgeBase {
        &mut self.base
    }
}

/// A single step in a workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// Execution order (1-based)
    pub order: u32,
    /// Human-readable description of what this step does
    pub description: String,
    /// Shell command to execute (if any)
    #[serde(default)]
    pub command: Option<String>,
    /// Tool to use (e.g. "cargo", "npm")
    #[serde(default)]
    pub tool: Option<String>,
    /// Whether this step requires user approval before executing
    #[serde(default)]
    pub needs_approval: bool,
    /// What to do if this step fails
    #[serde(default)]
    pub on_failure: FailureAction,
}

/// What to do when a workflow step fails.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum FailureAction {
    /// Skip this step and continue
    Skip,
    /// Abort the entire workflow
    #[default]
    Abort,
    /// Retry the step
    Retry,
}

/// A variable/parameter for a workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Variable {
    /// Variable name
    pub name: String,
    /// Type of the variable
    #[serde(rename = "type", default)]
    pub var_type: VarType,
    /// Whether this variable must be provided
    #[serde(default)]
    pub required: bool,
    /// Default value (as string)
    #[serde(default)]
    pub default_value: Option<String>,
    /// Human-readable description
    #[serde(default)]
    pub description: String,
}

/// Variable types for workflow parameters.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum VarType {
    #[default]
    String,
    Path,
    Url,
    Number,
    Bool,
}

/// Permission level for workflow execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    /// Read-only access
    #[default]
    Read,
    /// Read and write access
    Write,
    /// Execute only (no read/write of intermediate state)
    #[serde(rename = "execute_only")]
    ExecuteOnly,
}
