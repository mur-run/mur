//! Built-in `open_item` tool: let an agent record what it knows is unfinished.
//!
//! This is the `Reported` half of open items — the agent's word, stored so the
//! user sees it after the conversation that produced it has scrolled away.
//! It writes only to `<mur_home>/open-items.jsonl`, which the sandbox policy
//! allowlists by name, so an agent needs no filesystem grant over `~/.mur` to
//! use it.
//!
//! No allowlist gate, unlike `fleet_run`. That tool spawns processes and spends
//! money; this one appends a line of text to a log the user reads. The blast
//! radius of the worst case — an agent that writes nonsense items — is a user
//! deleting a line, and the display already marks every item here as the
//! agent's unverified claim.

use crate::tools::{ToolDef, ToolError, ToolExecutor, ToolOutput};

pub const OPEN_ITEM: &str = "open_item";

/// Cap per call. An agent that thinks it owes twenty things is not reporting,
/// it is dumping, and a panel that can be flooded is a panel nobody reads.
const MAX_TITLE_LEN: usize = 200;

/// Whether a title reads as having a due time.
///
/// An agent asked to remember "remind me at 10:00 tomorrow" reaches for this
/// tool because it is the one in hand, and writes an item that can only ever
/// expire — `mur agent schedule` is the surface that fires. This does not
/// block that; it appends one advisory sentence so the agent can correct
/// itself in the same turn.
///
/// Deliberately narrow, and in both languages these agents are used in. A miss
/// costs nothing (the item is still recorded exactly as before); a false
/// positive costs one sentence.
fn looks_time_bound(title: &str) -> bool {
    const WORDS: &[&str] = &[
        "tomorrow",
        "tonight",
        "next week",
        "remind me",
        "o'clock",
        "明天",
        "後天",
        "今晚",
        "下週",
        "下星期",
        "提醒",
        "鬧鐘",
    ];
    let lower = title.to_lowercase();
    if WORDS.iter().any(|w| lower.contains(w)) {
        return true;
    }
    // A clock time: `10:00`, or `10 點` in the CJK form. Whitespace is stripped
    // first — `8 點` is written with a space at least as often as without.
    let c: Vec<char> = title.chars().filter(|c| !c.is_whitespace()).collect();
    c.windows(3)
        .any(|w| w[0].is_ascii_digit() && w[1] == ':' && w[2].is_ascii_digit())
        || c.windows(2).any(|w| w[0].is_ascii_digit() && w[1] == '點')
}

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
(queued jobs and pending proposals are surfaced automatically). This list has no clock: nothing \
recorded here ever fires, so a request with a due time belongs in `mur agent schedule` as well. \
Pass `resolve` with an id from a previous call to clear one."
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

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        if let Some(id) = input.get("resolve").and_then(|v| v.as_str()) {
            mur_open_items::resolve(&self.mur_home, id)
                .map_err(|e| ToolError::Execution(format!("resolve open item: {e:#}")))?;
            return Ok(format!("Resolved open item {id}.").into());
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

        // `{e:#}`, not `{e}`: anyhow's plain Display prints only the outermost
        // context, so an io failure here reached the model as a bare
        // `open <path>` with the errno dropped — the one detail that separates
        // "the sandbox denied it" from "the disk is full".
        let id = mur_open_items::report(&self.mur_home, &self.agent_name, title, next)
            .map_err(|e| ToolError::Execution(format!("record open item: {e:#}")))?;

        Ok(format!(
            "Recorded as {id}. The user sees it under \"reported\" in `mur open`, marked as your \
unverified claim.{}",
            if looks_time_bound(title) {
                " This title reads as time-bound, and open items have no clock — nothing here \
fires. If something must happen at a time, set it with `mur agent schedule` too; this item only \
records that you owe it."
            } else {
                ""
            }
        )
        .into())
    }
}

#[cfg(test)]
mod tests {
    /// The item from #1075, verbatim. It sat for three weeks and expired.
    #[test]
    fn the_reminder_that_could_never_fire_is_flagged() {
        assert!(looks_time_bound("明天早上 10:00 提醒使用者吃早餐"));
    }

    #[test]
    fn time_bound_titles_are_caught_in_both_languages() {
        for t in [
            "remind me to check the deploy",
            "ping the team tomorrow",
            "call at 09:30",
            "後天要回覆",
            "晚上 8 點開始跑",
        ] {
            assert!(looks_time_bound(t), "missed: {t}");
        }
    }

    /// The control that matters: ordinary unfinished work must not collect an
    /// irrelevant sentence about scheduling on every single call.
    #[test]
    fn ordinary_work_items_are_not_flagged() {
        for t in [
            "Commit and push the signing-cert bump",
            "等 pm 交出重做版統一聊天 IA 設計規格",
            "write the tests for the merge path",
            "確認 drs-ux 產出 design2.md",
        ] {
            assert!(!looks_time_bound(t), "false positive: {t}");
        }
    }

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
        let id = out
            .text
            .split_whitespace()
            .nth(2)
            .unwrap()
            .trim_end_matches('.');
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
