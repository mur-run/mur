//! The guarded tool-call sequence: the one place a MUR tool runs.
//!
//! Every obligation here fails silently when it is skipped — a missing mask
//! leaks instead of erroring, a missing task scope orphans a `bash` job
//! instead of erroring. `task_runner.rs` carried two comments warning that a
//! third execution site would break exactly those two things. This type is
//! the answer: one owner, so "is the rule kept?" is a question about one
//! place instead of a question about vigilance.
//!
//! It exists to have a second caller — the MCP server that serves MUR's
//! tools to a spawned CLI — without that caller becoming a second path.
//! See `docs/superpowers/specs/2026-09-16-mur-tool-mcp-server-design.md`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::hitl::HitlApprovals;

/// Everything the guarded sequence reads. Cloned from `TaskRunner` rather
/// than borrowed: the MCP server needs one that outlives any single turn's
/// borrow, and cloning `Arc`s and a small `Vec` per call is not a cost worth
/// a lifetime parameter here.
pub struct GuardedToolCall {
    pub(crate) tools: Vec<Arc<dyn crate::tools::ToolExecutor>>,
    pub(crate) tools_policy: Vec<mur_common::agent::ToolRule>,
    pub(crate) secrets: Option<Arc<crate::secrets::SecretVault>>,
    pub(crate) notifier: Option<tokio::sync::mpsc::Sender<serde_json::Value>>,
    pub(crate) client_notifiers:
        Arc<tokio::sync::Mutex<HashMap<String, crate::task_runner::ApprovalSink>>>,
    pub(crate) agent_name: String,
    pub(crate) decision_store: Option<Arc<dyn crate::hitl::store::DecisionStore>>,
    pub(crate) hitl_timeout_secs: u32,
    pub(crate) pending_approvals: Option<HitlApprovals>,
}
