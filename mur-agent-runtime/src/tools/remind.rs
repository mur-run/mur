//! Built-in `remind` tool: let an agent ask for a schedule it cannot create.
//!
//! An agent's schedules live in `lifecycle.schedule` inside its own
//! `profile.yaml`, and an agent may not write that file — the sandbox denies it
//! unconditionally, so a running agent cannot widen its own entitlements and
//! restart into them. "Remind me at 10 tomorrow" therefore cannot become a
//! schedule from the inside, however well the agent understands the request.
//!
//! Before this, that request became an open item: free text, no due field,
//! nothing to fire it. The one in issue #1075 sat for three weeks and was
//! closed by hand as expired.
//!
//! Now it becomes a proposal — a file in the agent's own home, which it may
//! write — that `mur agent schedule accept` turns into the real entry, on the
//! real scheduler. The agent does not gain the ability to wake itself up; it
//! gains the ability to ask, in a form a person can act on with one command.
//!
//! The cron expression comes from the model, not from a date parser here.
//! Converting "tomorrow at 10" into `0 10 <dom> <mon> *` needs today's date and
//! the user's intent, both of which the model has and this crate does not.

use crate::tools::{ToolDef, ToolError, ToolExecutor, ToolOutput};

pub const REMIND: &str = "remind";

pub struct RemindTool {
    pub agent_home: std::path::PathBuf,
}

#[async_trait::async_trait]
impl ToolExecutor for RemindTool {
    fn name(&self) -> &str {
        REMIND
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: REMIND.into(),
            description: "Ask for a recurring or one-off reminder that MUR's scheduler will \
actually fire. Use this whenever a request has a time in it — \"remind me at 10 tomorrow\", \
\"every weekday morning\", \"in an hour\". Recording a timed request as an open item instead \
leaves it with nothing to fire it. This writes a PROPOSAL: the user runs `mur agent schedule \
accept` to grant it, so say that you have asked rather than that it is set."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "required": ["cron", "message"],
                "properties": {
                    "cron": {
                        "type": "string",
                        "description": "Standard 5-field cron in the user's LOCAL time: minute hour day-of-month month day-of-week. Resolve relative words yourself — 'tomorrow at 10' is `0 10 <tomorrow's day> <month> *`, not a phrase. Use `*` in day-of-month and month for a recurring one."
                    },
                    "message": {
                        "type": "string",
                        "description": "What to say when it fires, written as the reminder itself"
                    },
                    "asked_for": {
                        "type": "string",
                        "description": "The user's own words, verbatim. A cron expression is not reviewable on its own and the reviewer is being asked whether this is what they meant."
                    }
                }
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let field = |k: &str| -> Option<String> {
            input
                .get(k)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let cron = field("cron").ok_or_else(|| {
            ToolError::InvalidInput("provide `cron` — 5 fields, local time".into())
        })?;
        let message = field("message").ok_or_else(|| {
            ToolError::InvalidInput("provide `message` — what to say when it fires".into())
        })?;

        // Rejected here rather than at accept time: an agent that writes an
        // unparseable proposal has told the user a reminder is pending, and the
        // failure surfaces only when they try to grant it.
        if cron.split_whitespace().count() != 5 {
            return Err(ToolError::InvalidInput(format!(
                "`cron` needs 5 space-separated fields (minute hour day-of-month month \
                 day-of-week), got {:?}",
                cron
            )));
        }

        let dir = self
            .agent_home
            .join(mur_common::agent::SCHEDULE_PROPOSAL_DIR);
        std::fs::create_dir_all(&dir)
            .map_err(|e| ToolError::Execution(format!("create proposal dir: {e}")))?;

        let proposal = mur_common::agent::ScheduleProposal {
            cron: cron.clone(),
            message: message.clone(),
            asked_for: field("asked_for"),
            proposed_at: chrono::Utc::now().to_rfc3339(),
        };
        // Derived from the content, so the same request restated in one
        // conversation folds onto one proposal instead of stacking.
        let id = proposal_id(&cron, &message);
        let body = serde_yaml_ng::to_string(&proposal)
            .map_err(|e| ToolError::Execution(format!("serialise proposal: {e}")))?;
        std::fs::write(dir.join(format!("{id}.yaml")), body)
            .map_err(|e| ToolError::Execution(format!("write proposal: {e}")))?;

        Ok(format!(
            "Asked for a reminder ({id}): {cron:?} → {message:?}. This is a PROPOSAL — an agent \
             cannot create its own schedule. It fires once the user runs:\n    mur agent schedule \
             accept <agent> {id}\nSay that you have asked, not that it is set."
        )
        .into())
    }
}

/// Stable across restatements of the same request within a conversation.
fn proposal_id(cron: &str, message: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(cron.as_bytes());
    h.update(b"\0");
    h.update(message.trim().to_lowercase().as_bytes());
    format!("{:x}", h.finalize())[..12].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(dir: &std::path::Path) -> RemindTool {
        RemindTool {
            agent_home: dir.to_path_buf(),
        }
    }

    #[tokio::test]
    async fn a_proposal_lands_where_the_cli_looks_for_it() {
        let d = tempfile::tempdir().unwrap();
        let out = tool(d.path())
            .execute(serde_json::json!({
                "cron": "0 10 9 8 *",
                "message": "吃早餐",
                "asked_for": "明天早上 10:00 提醒我吃早餐"
            }))
            .await
            .unwrap();
        assert!(out.text.contains("PROPOSAL"), "{}", out.text);
        assert!(out.text.contains("schedule accept"), "{}", out.text);

        let dir = d.path().join(mur_common::agent::SCHEDULE_PROPOSAL_DIR);
        let files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(files.len(), 1, "{files:?}");
        let p: mur_common::agent::ScheduleProposal =
            serde_yaml_ng::from_str(&std::fs::read_to_string(files[0].path()).unwrap()).unwrap();
        assert_eq!(p.cron, "0 10 9 8 *");
        // The user's words survive: a cron expression alone is not reviewable.
        assert_eq!(p.asked_for.as_deref(), Some("明天早上 10:00 提醒我吃早餐"));
    }

    /// Restating the same reminder must not stack proposals — the same failure
    /// `open_item`'s id-folding exists to prevent.
    #[tokio::test]
    async fn the_same_request_twice_is_one_proposal() {
        let d = tempfile::tempdir().unwrap();
        for _ in 0..2 {
            tool(d.path())
                .execute(serde_json::json!({"cron": "0 9 * * 1-5", "message": "standup"}))
                .await
                .unwrap();
        }
        let dir = d.path().join(mur_common::agent::SCHEDULE_PROPOSAL_DIR);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    }

    /// A malformed cron must fail here, not at accept time: the agent has
    /// already told the user a reminder is pending by then.
    #[tokio::test]
    async fn a_cron_that_is_not_five_fields_is_refused_at_the_tool() {
        let d = tempfile::tempdir().unwrap();
        let err = tool(d.path())
            .execute(serde_json::json!({"cron": "tomorrow at 10", "message": "x"}))
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("5 space-separated"), "{err}");
        assert!(
            !d.path()
                .join(mur_common::agent::SCHEDULE_PROPOSAL_DIR)
                .exists(),
            "nothing may be written for a refused proposal"
        );
    }
}
