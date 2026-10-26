//! Turn-start half of MCP hot reload: refresh the tool set and hold the
//! turn's notices until its reply is written (spec 2026-10-25 §7 turn order).
//!
//! Notices go in the reply's footer, like a refused cwd: every client shows the
//! reply, while a mid-stream frame would land inside text the CLI matches
//! against the final reply.

use super::*;

/// One turn's MCP reload notices, held until its reply is written.
pub(super) struct McpNotices {
    pub(super) text: String,
    /// Attach a restart chip: one of the notices only a restart applies.
    pub(super) restart: bool,
}

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
                .insert(
                    turn.to_string(),
                    McpNotices {
                        text: out.notices.join("\n"),
                        restart: out.restart,
                    },
                );
        }
    }

    /// Remove turn `turn`'s MCP reload notices for the reply footer.
    pub(super) fn take_mcp_notices(&self, turn: &str) -> Option<McpNotices> {
        self.mcp_notices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(turn)
    }
}

impl McpNotices {
    /// Append the notices to `reply`, plus a restart chip when one applies.
    /// The chip is a `propose`-shaped Data part, so every client runs the same
    /// `vet` on it as on a model's own `propose` call; the text still names
    /// the command for a client that shows no chip.
    pub(super) fn apply(self, reply: Message) -> Message {
        let mut reply = super::run::append_note(reply, &self.text);
        if self.restart {
            reply.parts.push(MessagePart::Data {
                mime_type: mur_common::proposal::PROPOSAL_MIME.into(),
                data: mur_common::proposal::restart_args(crate::tools::refresh::RESTART_LABEL),
            });
        }
        reply
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_reply() -> Message {
        Message {
            role: "agent".into(),
            parts: vec![MessagePart::Text {
                text: "done".into(),
            }],
        }
    }

    fn parts_json(m: &Message) -> serde_json::Value {
        serde_json::to_value(&m.parts).expect("parts serialize")
    }

    #[test]
    fn a_restart_notice_attaches_a_chip_every_client_vets() {
        let reply = McpNotices {
            text: "MCP `x` edit to url needs a restart.".into(),
            restart: true,
        }
        .apply(text_reply());
        let p = mur_common::proposal::attached(&parts_json(&reply)).expect("chip");
        assert_eq!(p.kind, mur_common::proposal::ProposalKind::Restart);
        assert_eq!(p.label, crate::tools::refresh::RESTART_LABEL);
        let MessagePart::Text { text } = &reply.parts[0] else {
            panic!("text first");
        };
        assert!(text.ends_with("needs a restart."), "{text}");
    }

    #[test]
    fn a_notice_without_restart_adds_text_only() {
        let reply = McpNotices {
            text: "MCP `x` reloaded: added `y`".into(),
            restart: false,
        }
        .apply(text_reply());
        assert_eq!(reply.parts.len(), 1);
        assert!(mur_common::proposal::attached(&parts_json(&reply)).is_none());
    }
}
