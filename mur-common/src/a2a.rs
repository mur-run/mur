//! A2A v0.3 protocol envelope types.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String, // "user" | "agent" | "system"
    pub parts: Vec<MessagePart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum MessagePart {
    Text {
        text: String,
    },
    Data {
        #[serde(rename = "mimeType")]
        mime_type: String,
        data: serde_json::Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    Submitted,
    Working,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub state: TaskState,
    pub messages: Vec<Message>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "completedAt", skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TaskError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<serde_json::Value>,
    /// Zero or more file artifacts produced by this task. Present when the
    /// task completed with an `output_artifact_path` — the consumer reads
    /// these files byte-by-byte instead of re-typing inline content through
    /// another LLM (#715 Part B).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<Vec<ArtifactInfo>>,
}

/// Metadata for an artifact produced by a task — a file the agent wrote to
/// a known path so callers can read it byte-by-byte instead of re-typing the
/// content through another LLM (issue #715 Part B).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactInfo {
    /// Absolute path the artifact was written to.
    pub path: String,
    /// MIME type (e.g. "text/markdown", "application/json").
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    /// SHA-256 hex digest of the file content, for integrity checking by the
    /// consumer before they use it. Always present when the runtime verified
    /// the file post-completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// File size in bytes.
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskError {
    pub code: String,
    pub message: String,
    pub recoverable: bool,
    /// Boxed so `TaskError` stays small as the `Err` of hot `Result`s:
    /// with serde_json's `preserve_order` (pulled in by lancedb ≥ 0.41)
    /// an inline `Value` pushes it past clippy's `result_large_err` limit.
    /// `Box` is transparent to serde, so the wire format is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Box<serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String, // always "2.0"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>, // None = notification
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String, // "2.0"
    pub id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::TaskError;

    /// Boxing `details` must not change the wire format: peers on older
    /// builds send and expect a bare JSON value here.
    #[test]
    fn task_error_details_wire_format_unchanged() {
        let wire = r#"{"code":"x","message":"m","recoverable":false,"details":{"k":[1,2]}}"#;
        let err: TaskError = serde_json::from_str(wire).unwrap();
        assert_eq!(
            err.details.as_deref(),
            Some(&serde_json::json!({"k": [1, 2]}))
        );
        assert_eq!(serde_json::to_string(&err).unwrap(), wire);

        let bare = r#"{"code":"x","message":"m","recoverable":true}"#;
        let err: TaskError = serde_json::from_str(bare).unwrap();
        assert!(err.details.is_none());
        assert_eq!(serde_json::to_string(&err).unwrap(), bare);
    }
}
