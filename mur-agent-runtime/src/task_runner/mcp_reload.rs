//! Turn-start half of MCP hot reload: refresh the tool set and hold the
//! turn's notices until its reply is written (spec 2026-10-25 §7 turn order).
//!
//! Notices go in the reply's footer, like a refused cwd: every client shows the
//! reply, while a mid-stream frame would land inside text the CLI matches
//! against the final reply.

use super::*;

impl TaskRunner {
    /// Re-list hot-reloaded MCP servers and record this turn's notices. Runs
    /// once per turn, after the profile stamp check and before the model is
    /// called, so the turn sees one tool set from start to end.
    pub(super) async fn refresh_mcp_tools(&self, turn: &str) {
        let Some(refresh) = &self.mcp_refresh else {
            return;
        };
        let live = self.live.get().cloned();
        // Read first: a stale profile is re-read here, so the pool and the
        // view below see the same reload.
        if let Some(l) = &live {
            l.profile.current();
        }
        let current = self.tools();
        let out = refresh
            .refresh(&current, &self.tools_policy, || {
                let state = live.as_ref().map(|l| l.profile.state()).unwrap_or_default();
                crate::tools::refresh::ReloadView {
                    refusals: state.mcp_refusals,
                    cold: state.pending_cold_restart,
                }
            })
            .await;
        if let Some(tools) = out.tools {
            *self.tools.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(tools);
        }
        if !out.notices.is_empty() {
            self.mcp_notices
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(turn.to_string(), out.notices);
        }
    }

    /// Remove and render turn `turn`'s MCP reload notices for the reply footer.
    pub(super) fn take_mcp_notices(&self, turn: &str) -> Option<String> {
        let notices = self
            .mcp_notices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(turn)?;
        Some(notices.join("\n"))
    }
}
