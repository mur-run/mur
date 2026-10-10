//! `git_push_status` and `git_push_cancel`. Neither moves a request: status reads
//! what the daemon published, cancel leaves a marker the daemon acts on.

use std::path::PathBuf;

use serde::Deserialize;

use mur_git_broker::request_file::validate_id;

use super::{GIT_PUSH_CANCEL, GIT_PUSH_STATUS, invalid};
use crate::llm::ToolDef;
use crate::tools::{ToolError, ToolExecutor, ToolOutput};

/// Larger than any status the daemon writes; a bigger file is not one of ours.
const MAX_STATUS_BYTES: u64 = 16 * 1024;
const CANCEL_EXT: &str = "cancel";
const STATUS_EXT: &str = "yaml";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdArg {
    request_id: String,
}

fn parse(input: serde_json::Value) -> Result<String, ToolError> {
    let a: IdArg = serde_json::from_value(input).map_err(|e| invalid(&format!("schema: {e}")))?;
    validate_id(&a.request_id, "request_id").map_err(|_| invalid("request_id"))?;
    Ok(a.request_id)
}

fn id_schema(tool: &str, description: &str) -> ToolDef {
    ToolDef {
        name: tool.into(),
        description: description.into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"request_id": {"type": "string", "description": "The id you minted for git_push_request."}},
            "required": ["request_id"],
            "additionalProperties": false
        }),
    }
}

pub struct GitPushStatusTool {
    /// `<mur_home>/git-push/status/<agent>`: daemon-written, agent-readable.
    status_dir: PathBuf,
}

impl GitPushStatusTool {
    pub fn new(status_dir: PathBuf) -> Self {
        Self { status_dir }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for GitPushStatusTool {
    fn name(&self) -> &str {
        GIT_PUSH_STATUS
    }

    fn def(&self) -> ToolDef {
        id_schema(
            GIT_PUSH_STATUS,
            "Status of a git_push_request, as the broker last published it: pending_approval, \
pushed, rejected (with a code), outcome_unknown, cancelled, … `unknown` means the broker has not \
published anything for this id yet.",
        )
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let id = parse(input)?;
        let path = self.status_dir.join(format!("{id}.{STATUS_EXT}"));
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(format!("request {id}: unknown — no status published yet").into());
            }
            Err(e) => return Err(ToolError::Execution(format!("status: {}", e.kind()))),
        };
        if !meta.is_file() || meta.len() > MAX_STATUS_BYTES {
            return Err(ToolError::Execution(
                "status file is not one the broker wrote".into(),
            ));
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| ToolError::Execution(format!("status: {}", e.kind())))?;
        Ok(format!("request {id}:\n{text}").into())
    }
}

pub struct GitPushCancelTool {
    /// `<agent_home>/inbox/git-push-cancel`.
    cancel_dir: PathBuf,
}

impl GitPushCancelTool {
    pub fn new(cancel_dir: PathBuf) -> Self {
        Self { cancel_dir }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for GitPushCancelTool {
    fn name(&self) -> &str {
        GIT_PUSH_CANCEL
    }

    fn def(&self) -> ToolDef {
        id_schema(
            GIT_PUSH_CANCEL,
            "Ask the broker to cancel a git_push_request. Answers `pending_cancel`: the broker \
decides, and a push already executing cannot be recalled. Confirm with git_push_status.",
        )
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let id = parse(input)?;
        std::fs::create_dir_all(&self.cancel_dir)
            .map_err(|e| ToolError::Execution(format!("cancel: {}", e.kind())))?;
        let marker = self.cancel_dir.join(format!("{id}.{CANCEL_EXT}"));
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&marker)
            .map_err(|e| ToolError::Execution(format!("cancel: {}", e.kind())))?;
        Ok(
            format!("request {id}: pending_cancel — the broker decides; check git_push_status")
                .into(),
        )
    }
}
