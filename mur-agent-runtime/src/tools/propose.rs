//! `propose` — the agent offers the user ONE command or native action, shown
//! by murmur as a chip under the composer. Like `suggest_replies`, the
//! user-facing effect rides on the streamed tool-call args; unlike it, the
//! executor vets the args and returns a tool error on rejection, so the model
//! learns why and can fix it. Vetting is `mur_common::proposal::vet`, the same
//! function murmur runs before rendering.

use super::{ToolError, ToolExecutor, ToolOutput};
use crate::llm::ToolDef;
use mur_common::proposal::{
    COMMAND_MAX_CHARS, KIND_RESTART, KIND_SHELL, KIND_SLASH, KINDS, LABEL_MAX_CHARS, PROPOSE_TOOL,
    vet,
};

pub struct ProposeTool;

#[async_trait::async_trait]
impl ToolExecutor for ProposeTool {
    fn name(&self) -> &str {
        PROPOSE_TOOL
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: PROPOSE_TOOL.into(),
            description: format!(
                "Propose ONE command or action for the user instead of telling them to \
                type it. It appears as a chip under their input; nothing runs until they \
                act. Kinds: `{KIND_SHELL}` (a shell command; the user presses Tab to put \
                it in their input, reviews it, and sends it), `{KIND_SLASH}` (a slash \
                command, same Tab flow), `{KIND_RESTART}` (restart YOU, the current agent, \
                e.g. after your profile changed; the user presses Enter to run it; takes \
                no command and cannot target another agent). The command must be a \
                single line with concrete values — no placeholders like <name> or \
                {{agent}}, no secrets. Your message must still say what the command is \
                for; the chip is a shortcut, not the explanation. A later call replaces \
                the chip."
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "label": {
                        "type": "string",
                        "maxLength": LABEL_MAX_CHARS,
                        "description": "Short description shown on the chip, e.g. \"restart to apply the new model\"."
                    },
                    "kind": {
                        "type": "string",
                        "enum": KINDS,
                        "description": "shell / slash are inserted for the user to review; restart runs on Enter."
                    },
                    "command": {
                        "type": "string",
                        "maxLength": COMMAND_MAX_CHARS,
                        "description": "Required for shell and slash, forbidden for restart. The leading `!` or `/` is optional."
                    }
                },
                "required": ["label", "kind"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        match vet(&input) {
            Ok(_) => Ok("ok — shown to the user as a proposal".to_string().into()),
            Err(e) => Err(ToolError::InvalidInput(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn accepted_proposal_is_ok() {
        let out = ProposeTool
            .execute(json!({"label": "restart to apply", "kind": "restart"}))
            .await;
        assert!(out.is_ok());
    }

    #[tokio::test]
    async fn rejected_proposal_is_a_tool_error_the_model_reads() {
        let out = ProposeTool
            .execute(json!({"label": "x", "kind": "shell", "command": "mur agent stop <name>"}))
            .await;
        match out {
            Err(ToolError::InvalidInput(msg)) => assert!(msg.contains("<name>"), "{msg}"),
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[test]
    fn def_has_canonical_name_and_kind_enum() {
        let d = ProposeTool.def();
        assert_eq!(d.name, "propose");
        assert_eq!(
            d.input_schema["properties"]["kind"]["enum"],
            json!(["shell", "slash", "restart"])
        );
        assert!(
            d.description.contains("{agent}"),
            "braces must survive format!"
        );
    }
}
