pub mod bash;
mod denial;
pub mod edit_file;
pub mod fleet_run;
pub(crate) mod fs_policy;
pub mod mcp;
pub mod naming;
pub mod open_item;
pub mod read_file;
pub mod recall;
pub mod registry;
pub mod remember;
pub mod remind;
pub(crate) mod suggest;
pub mod write_file;

use crate::llm::ToolDef;

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("tool execution failed: {0}")]
    Execution(String),
    #[error("unknown tool: {0}")]
    Unknown(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

/// Structural status of a tool execution.
///
/// This exists because the bash tool used to encode exit code and sandbox
/// denial as text markers (e.g. `"[exit code: N]"`, `"[sandbox] ..."`) glued
/// onto the end of its plain-`String` output. Downstream code then grepped
/// those markers back out of the string. That let file *content* forge
/// execution status: reading a file that merely happened to contain the
/// marker text made a successful read render as a sandbox denial. Status now
/// travels as data, structurally, alongside the text - never inside it.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ToolStatus {
    #[default]
    Ok,
    Failed {
        exit_code: i32,
    },
    Denied {
        detail: String,
    },
}

/// Result of a tool execution: the model-facing text plus the real,
/// structural execution status (see [`ToolStatus`]).
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub text: String,
    pub status: ToolStatus,
    /// Images the tool produced, handed to the model as real vision input
    /// rather than described in prose.
    ///
    /// Before this existed a tool could only return text, so an agent that
    /// fetched a photo had no way to show it to its own model — the runtime
    /// was the missing link between MCP (whose results already carry image
    /// content) and the Messages API (whose `tool_result` already accepts
    /// image blocks). Adapters that cannot express images in a tool result
    /// drop these and keep `text`; see `llm::openai`.
    pub images: Vec<ToolImage>,
}

/// One image carried out of a tool call.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolImage {
    /// e.g. "image/jpeg" — passed through to the provider unchanged.
    pub media_type: String,
    /// Base64-encoded bytes, with no `data:` prefix.
    pub data: String,
}

/// Media types a vision model will accept. A tool that produces anything
/// else keeps it as text: handing the provider an unsupported media type
/// fails the whole turn, which is worse than losing the picture.
pub const SUPPORTED_IMAGE_MEDIA_TYPES: [&str; 4] =
    ["image/jpeg", "image/png", "image/gif", "image/webp"];

/// Ceiling on a single decoded image, mirroring the provider's own limit.
/// Base64 inflates by 4/3, so this lands just under the 5 MB wire cap.
pub const MAX_IMAGE_BYTES: usize = 3_750_000;

impl ToolImage {
    /// True when this image is small enough and of a type the provider takes.
    pub fn is_supported(&self) -> bool {
        SUPPORTED_IMAGE_MEDIA_TYPES.contains(&self.media_type.as_str())
            && self.data.len() <= MAX_IMAGE_BYTES.div_ceil(3) * 4
    }
}

impl From<String> for ToolOutput {
    fn from(text: String) -> Self {
        ToolOutput {
            text,
            status: ToolStatus::Ok,
            images: Vec::new(),
        }
    }
}

#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    fn name(&self) -> &str;
    fn def(&self) -> ToolDef;
    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::fs_policy::SessionCwd;
    use mur_common::agent::FilesystemEntitlement;

    fn cwd() -> SessionCwd {
        SessionCwd::new(std::path::PathBuf::from("/tmp"))
    }

    fn ent() -> FilesystemEntitlement {
        FilesystemEntitlement {
            read: vec![],
            write: vec![],
            deny: vec![],
        }
    }

    /// Every path-taking tool must advertise every form `fs_policy::resolve_path`
    /// actually accepts — including `~`.
    ///
    /// When the schema lists fewer forms than the resolver implements, the model
    /// expands `~` itself; and since nothing tells it what `~` is, it invents a
    /// username. Observed in the wild: agents wrote to `/Users/i/.mur/...` and
    /// `/Users/lidj/.mur/...` while the real home was `/Users/david`. The
    /// output-locations rule in the system prompt tells the model to write under
    /// `~/.mur/artifacts/`, so a schema that hides `~` puts two MUR-authored
    /// strings in direct contradiction.
    #[test]
    fn path_taking_tools_advertise_tilde() {
        let defs = [
            write_file::WriteFileTool::new_for_test(cwd(), ent()).def(),
            read_file::ReadFileTool::new_for_test(cwd(), ent()).def(),
            edit_file::EditFileTool::new_for_test(cwd(), ent()).def(),
        ];
        for d in defs {
            let desc = d.input_schema["properties"]["path"]["description"]
                .as_str()
                .unwrap_or_default();
            assert!(
                desc.contains('~'),
                "{}: path description hides `~` support, so the model will expand \
                 it itself and guess a home directory: {desc:?}",
                d.name
            );
        }
    }
}
