//! Built-in `open_item` tool: let an agent record what it knows is unfinished.
//!
//! This is the `Reported` half of open items — the agent's word, stored so the
//! user sees it after the conversation that produced it has scrolled away.
//! It writes only to `<mur_home>/open-items.jsonl`, so an agent needs no
//! filesystem grant over `~/.mur` to use it.
//!
//! No allowlist gate, unlike `fleet_run`. That tool spawns processes and spends
//! money; this one appends a line of text to a log the user reads. The blast
//! radius of the worst case — an agent that writes nonsense items — is a user
//! deleting a line, and the display already marks every item here as the
//! agent's unverified claim.

use crate::tools::{ToolDef, ToolError, ToolExecutor};

pub const OPEN_ITEM: &str = "open_item";

/// Cap per call. An agent that thinks it owes twenty things is not reporting,
/// it is dumping, and a panel that can be flooded is a panel nobody reads.
const MAX_TITLE_LEN: usize = 200;

pub struct OpenItemTool {
    pub mur_home: std::path::PathBuf,
    pub agent_name: String,
}

#[async_trait::async_trait]
impl ToolExecutor for OpenItemTool {
    fn name(&self) -> &str {
        OPEN_ITEM
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: OPEN_ITEM.into(),
            description: "Record something left unfinished, so the user still sees it after this \
conversation scrolls away. Use for work you agreed to and did not complete, a blocker you hit, or \
a decision you are waiting on — not for things you just did, and not for what MUR already tracks \
(queued jobs and pending proposals are surfaced automatically). Pass `resolve` with an id from a \
previous call to clear one."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "One line, imperative. Repeating the same title later updates that item rather than adding another."
                    },
                    "next": {
                        "type": "string",
                        "description": "The command or place that resolves it, if there is an obvious one"
                    },
                    "resolve": {
                        "type": "string",
                        "description": "Id of an item to mark resolved instead of adding one"
                    }
                }
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String, ToolError> {
        if let Some(id) = input.get("resolve").and_then(|v| v.as_str()) {
            mur_open_items::resolve(&self.mur_home, id)
                .map_err(|e| ToolError::Execution(format!("resolve open item: {e}")))?;
            return Ok(format!("Resolved open item {id}."));
        }

        let title = input
            .get("title")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ToolError::InvalidInput("provide `title` to record, or `resolve` to clear".into())
            })?;

        if title.chars().count() > MAX_TITLE_LEN {
            return Err(ToolError::InvalidInput(format!(
                "title too long ({} chars, max {MAX_TITLE_LEN}) — one line, not a summary",
                title.chars().count()
            )));
        }

        let next = input
            .get("next")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let id = mur_open_items::report(&self.mur_home, &self.agent_name, title, next)
            .map_err(|e| ToolError::Execution(format!("record open item: {e}")))?;

        Ok(format!(
            "Recorded as {id}. The user sees it under \"reported\" in `mur open`, marked as your \
unverified claim."
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(home: &std::path::Path) -> OpenItemTool {
        OpenItemTool {
            mur_home: home.to_path_buf(),
            agent_name: "mur".into(),
        }
    }

    #[tokio::test]
    async fn records_and_then_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tool(tmp.path());

        let out = t
            .execute(serde_json::json!({"title": "write the tests", "next": "cargo test"}))
            .await
            .unwrap();
        assert_eq!(mur_open_items::open(tmp.path()).len(), 1);

        // The id is in the reply so the model can clear it next turn.
        let id = out.split_whitespace().nth(2).unwrap().trim_end_matches('.');
        t.execute(serde_json::json!({"resolve": id})).await.unwrap();
        assert!(mur_open_items::open(tmp.path()).is_empty());
    }

    /// An empty or missing title must not become a blank line in the user's
    /// panel — better a tool error the model can correct.
    #[tokio::test]
    async fn blank_title_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(
            tool(tmp.path())
                .execute(serde_json::json!({"title": "   "}))
                .await
                .is_err()
        );
        assert!(
            tool(tmp.path())
                .execute(serde_json::json!({}))
                .await
                .is_err()
        );
    }

    /// A panel that can be flooded with essays is a panel nobody reads.
    #[tokio::test]
    async fn an_essay_is_rejected_rather_than_truncated() {
        let tmp = tempfile::tempdir().unwrap();
        let long = "x".repeat(MAX_TITLE_LEN + 1);
        let err = tool(tmp.path())
            .execute(serde_json::json!({ "title": long }))
            .await
            .unwrap_err();
        assert!(format!("{err:?}").contains("too long"), "{err:?}");
        assert!(mur_open_items::open(tmp.path()).is_empty());
    }
}
