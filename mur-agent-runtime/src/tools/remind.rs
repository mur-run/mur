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
                    },
                    "once": {
                        "type": "boolean",
                        "description": "True when the request names ONE occasion — 'tomorrow at 10', 'on the 3rd', 'in an hour'. False for a recurrence — 'every weekday', 'each morning' — and also for a genuinely yearly one like a birthday. Cron has no year field, so a dated expression silently repeats every year; this is the only thing that stops it. Decide from the user's words, not from the cron: '0 9 15 3 *' is both 'remind me on March 15' and 'every year on my birthday', and the expression cannot tell you which."
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

        // A one-shot is bounded by its own first firing. The scheduler admits
        // the firing its bound names and retires the one after it, so this
        // fires exactly once (#1119). An expression with no future firing
        // yields no bound, which costs nothing: it never fires either.
        let not_after = crate::scheduler::one_off_bound(
            &cron,
            input.get("once").and_then(|v| v.as_bool()).unwrap_or(false),
        );

        let proposal = mur_common::agent::ScheduleProposal {
            cron: cron.clone(),
            message: message.clone(),
            asked_for: field("asked_for"),
            not_after,
            proposed_at: chrono::Utc::now().to_rfc3339(),
        };
        // Derived from the content, so the same request restated in one
        // conversation folds onto one proposal instead of stacking.
        let id = proposal_id(&cron, &message);
        // A correction supersedes what it corrects.
        //
        // The id is derived from the cron, so fixing a wrong date produces a
        // second file rather than replacing the first — and a live test did
        // exactly that: an agent guessed "tomorrow" wrong, checked `date`, and
        // asked again, leaving two proposals for one request, the abandoned one
        // due to fire six months later.
        //
        // Keyed on what the user said, not on what the agent computed: the
        // words are the request, and the cron is one attempt at expressing it.
        if let Some(asked) = proposal.asked_for.as_deref() {
            supersede(&dir, asked, &id);
        }
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

/// Remove earlier proposals for the same request, keeping `keep_id`.
///
/// Best-effort: a proposal that cannot be read is left alone rather than
/// deleted on a guess, and a failure here must not lose the new proposal — the
/// worst case is the duplicate this exists to prevent, which is visible in
/// `schedule proposals` and removable with `decline`.
fn supersede(dir: &std::path::Path, asked_for: &str, keep_id: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "yaml") {
            continue;
        }
        if path.file_stem().is_some_and(|s| s == keep_id) {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(old) = serde_yaml_ng::from_str::<mur_common::agent::ScheduleProposal>(&body) else {
            continue;
        };
        if old.asked_for.as_deref() == Some(asked_for) {
            let _ = std::fs::remove_file(&path);
        }
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
    /// The live test that produced this: an agent guessed "tomorrow" wrong,
    /// checked `date`, and asked again — leaving two proposals for one request,
    /// the abandoned one due to fire six months later. A reviewer should not
    /// have to work out which of two identical-looking asks is the live one.
    #[tokio::test]
    async fn a_corrected_reminder_supersedes_the_one_it_corrects() {
        let d = tempfile::tempdir().unwrap();
        let asked = "明天早上 10:00 提醒我吃早餐";
        // The wrong guess, then the correction, exactly as it happened.
        tool(d.path())
            .execute(
                serde_json::json!({"cron": "0 10 24 2 *", "message": "吃早餐", "asked_for": asked}),
            )
            .await
            .unwrap();
        tool(d.path())
            .execute(
                serde_json::json!({"cron": "0 10 1 9 *", "message": "吃早餐", "asked_for": asked}),
            )
            .await
            .unwrap();

        let dir = d.path().join(mur_common::agent::SCHEDULE_PROPOSAL_DIR);
        let files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(files.len(), 1, "one request, one live proposal: {files:?}");
        let p: mur_common::agent::ScheduleProposal =
            serde_yaml_ng::from_str(&std::fs::read_to_string(files[0].path()).unwrap()).unwrap();
        assert_eq!(
            p.cron, "0 10 1 9 *",
            "the correction is the one that survives"
        );
    }

    /// Two genuinely different requests are two proposals. Superseding on the
    /// user's words must not collapse unrelated reminders into one.
    #[tokio::test]
    async fn different_requests_stay_separate() {
        let d = tempfile::tempdir().unwrap();
        tool(d.path())
            .execute(serde_json::json!({"cron": "0 9 * * 1-5", "message": "standup", "asked_for": "weekday standup"}))
            .await
            .unwrap();
        tool(d.path())
            .execute(serde_json::json!({"cron": "0 18 * * 5", "message": "retro", "asked_for": "friday retro"}))
            .await
            .unwrap();
        let dir = d.path().join(mur_common::agent::SCHEDULE_PROPOSAL_DIR);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
    }

    /// Without the user's words there is nothing to key supersession on, and
    /// guessing from the message would collapse two reminders that happen to
    /// say the same thing at different times.
    #[tokio::test]
    async fn proposals_without_asked_for_are_left_alone() {
        let d = tempfile::tempdir().unwrap();
        for cron in ["0 10 1 9 *", "0 10 2 9 *"] {
            tool(d.path())
                .execute(serde_json::json!({"cron": cron, "message": "吃早餐"}))
                .await
                .unwrap();
        }
        let dir = d.path().join(mur_common::agent::SCHEDULE_PROPOSAL_DIR);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
    }

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

    async fn only_proposal(d: &std::path::Path) -> mur_common::agent::ScheduleProposal {
        let dir = d.join(mur_common::agent::SCHEDULE_PROPOSAL_DIR);
        let files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(files.len(), 1, "{files:?}");
        serde_yaml_ng::from_str(&std::fs::read_to_string(files[0].path()).unwrap()).unwrap()
    }

    /// The bug this whole field exists for: "remind me tomorrow at 10" has no
    /// year to put in a cron, so `0 10 1 9 *` means every September 1st. The
    /// bound is what turns that back into one morning (#1119).
    #[tokio::test]
    async fn a_one_off_is_bounded_by_its_own_first_firing() {
        let d = tempfile::tempdir().unwrap();
        tool(d.path())
            .execute(serde_json::json!({
                "cron": "0 10 1 9 *", "message": "breakfast",
                "asked_for": "tomorrow at 10", "once": true
            }))
            .await
            .unwrap();
        let p = only_proposal(d.path()).await;
        let first = crate::scheduler::next_n_fires("0 10 1 9 *", 1).unwrap()[0].to_rfc3339();
        assert_eq!(
            p.not_after.as_deref(),
            Some(first.as_str()),
            "the bound is the entry's own first firing — the scheduler admits \
             the firing its bound names and retires the next one"
        );
    }

    /// The control that keeps the bound from being applied to everything: a
    /// recurrence must stay unbounded, or every standup reminder dies after one.
    #[tokio::test]
    async fn a_recurring_request_carries_no_bound() {
        let d = tempfile::tempdir().unwrap();
        tool(d.path())
            .execute(serde_json::json!({
                "cron": "0 9 * * 1-5", "message": "standup", "asked_for": "every weekday"
            }))
            .await
            .unwrap();
        assert!(only_proposal(d.path()).await.not_after.is_none());
    }
}
