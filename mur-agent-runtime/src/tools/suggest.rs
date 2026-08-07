//! `suggest_replies` — a no-op tool the agent calls to offer the user 1–5
//! Tab-to-fill quick replies. The user-facing effect is carried entirely by the
//! streamed tool-call args (`StepStarted`); the executor itself does nothing and
//! returns a bare acknowledgement so the model can finish its turn.

use super::{ToolError, ToolExecutor, ToolOutput};
use crate::llm::ToolDef;

/// Canonical tool name. Shared by the runtime gate and the TUI interceptor.
pub const SUGGEST_REPLIES: &str = "suggest_replies";

pub struct SuggestRepliesTool;

#[async_trait::async_trait]
impl ToolExecutor for SuggestRepliesTool {
    fn name(&self) -> &str {
        SUGGEST_REPLIES
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: SUGGEST_REPLIES.into(),
            description: "Offer the user 1-5 short quick-reply options when they \
                would likely pick from a small set — e.g. after you ask a question \
                or propose a choice. Each option is a short candidate USER reply \
                (imperative, under ~60 characters) derived from your final message, \
                with an optional one-line description of the trade-off. Options are \
                ephemeral UI chrome, NOT an information channel: they MUST NOT \
                contain findings, analysis, or any information not already stated \
                in your final message text. Your final message must be \
                self-contained — state what you found first, then ask; a reader \
                who never sees the options must lose nothing. The options appear \
                as a chooser in the user's input. You decide what happens next: \
                if you genuinely need their answer before proceeding, end your \
                turn after offering the options and wait for their reply; if you \
                already know the next step (e.g. a plan they've approved), just \
                take it — don't offer a chooser in place of acting. Do NOT number \
                the options yourself (no \"A:\" / \"1.\" prefixes) — the UI marks \
                and spaces them. Skip it on open-ended turns with no natural \
                shortlist."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "replies": {
                        "type": "array",
                        "items": {
                            "oneOf": [
                                {
                                    "type": "string",
                                    "description": "A short candidate user reply (imperative, under ~60 chars), sent verbatim if picked. No new information."
                                },
                                {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "properties": {
                                        "text": {
                                            "type": "string",
                                            "description": "The short reply the user sends if they pick this option (imperative, under ~60 chars)."
                                        },
                                        "description": {
                                            "type": "string",
                                            "description": "Optional one-line trade-off shown under the option. May only restate information already in your final message — never new findings."
                                        }
                                    },
                                    "required": ["text"]
                                }
                            ]
                        },
                        "minItems": 1,
                        "maxItems": 5,
                        "description": "1-5 options, each a string or {text, description}."
                    }
                },
                "required": ["replies"]
            }),
        }
    }

    async fn execute(&self, _input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        // No side effects: the replies reach the user via the streamed args.
        Ok("ok".to_string().into())
    }
}

/// Whether `name` should be offered to the model this turn. Everything is
/// offered normally; `suggest_replies` is offered only on streaming
/// (interactive) turns so non-interactive callers never see it.
pub fn offer_for_streaming(name: &str, streaming: bool) -> bool {
    streaming || name != SUGGEST_REPLIES
}

/// `suggest_replies` is a no-side-effect built-in and is always auto-approved,
/// regardless of the agent's default tool policy.
pub fn suggest_replies_allowed(name: &str) -> bool {
    name == SUGGEST_REPLIES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn execute_is_noop_ok() {
        let out = SuggestRepliesTool
            .execute(serde_json::json!({ "replies": ["yes", "no"] }))
            .await;
        assert!(out.is_ok());
    }

    #[test]
    fn def_has_canonical_name_and_schema() {
        let d = SuggestRepliesTool.def();
        assert_eq!(d.name, "suggest_replies");
        assert_eq!(d.input_schema["properties"]["replies"]["type"], "array");
    }

    /// #716: the description must forbid using options as the information
    /// channel and require a self-contained final message, so the model can't
    /// bury its findings in ephemeral TUI chrome.
    #[test]
    fn def_constrains_options_to_final_message_content() {
        let d = SuggestRepliesTool.def();
        assert!(
            d.description
                .contains("MUST NOT contain findings, analysis, or any information"),
            "description must forbid findings/analysis in options"
        );
        assert!(
            d.description.contains("self-contained"),
            "description must require a self-contained final message"
        );
        assert!(
            d.description.contains("candidate USER reply"),
            "description must frame options as candidate user replies"
        );
    }

    #[test]
    fn streaming_gate() {
        assert!(offer_for_streaming("suggest_replies", true));
        assert!(!offer_for_streaming("suggest_replies", false));
        // Other tools are always offered.
        assert!(offer_for_streaming("bash", false));
        assert!(offer_for_streaming("bash", true));
    }

    #[test]
    fn policy_exemption_only_for_suggest() {
        assert!(suggest_replies_allowed("suggest_replies"));
        assert!(!suggest_replies_allowed("bash"));
    }
}
