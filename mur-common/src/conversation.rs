//! Shared conversation archive types (used by mur-core, mur-commander).
//!
//! JSONL row format version 1. See
//! `docs/superpowers/specs/2026-04-19-mur-conversations-design.md` §4.1.
//!
//! # Schema versioning
//!
//! Every `Message` carries `v: u32` (current: [`CONVERSATION_SCHEMA_VERSION`]).
//! This is intentional — schema evolution for a durable archive must be explicit.
//!
//! ## When to bump
//! Bump `CONVERSATION_SCHEMA_VERSION` ONLY when:
//!   1. A required field is renamed or removed, OR
//!   2. A field's semantic meaning changes (e.g., `ts` interpretation shifts
//!      from Utc to local), OR
//!   3. `Content` gains a variant that older deserializers cannot safely
//!      ignore.
//!
//! Adding a new optional field with `#[serde(default)]` does NOT require a bump.
//!
//! ## How to bump
//!   1. Add a schema migrator in `mur-core/src/conversations/migrate_schema.rs`
//!      that takes any JSON row and rewrites to the new schema.
//!   2. Wire it into the store's append/read paths so older lines in the same
//!      file still deserialize (via serde `untagged` or custom visitor).
//!   3. Keep the previous version's deserializer functional for at least one
//!      minor release.
//!   4. Bump `CONVERSATION_SCHEMA_VERSION`.
//!
//! ## Backward reads
//! `mur-core::conversations::store::read_day` must always migrate legacy rows
//! before constructing a `Message` so older on-disk JSONL still works.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Current conversations JSONL schema version. Bump only per the rules in this
/// module's top-level doc comment.
pub const CONVERSATION_SCHEMA_VERSION: u32 = 1;

/// One line in `~/.mur/conversations/raw/<date>/*.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Schema version; breaking changes bump this.
    pub v: u32,
    pub ts: DateTime<Utc>,
    pub src: Source,
    /// Conversation id; scopes messages within a source.
    pub conv: String,
    pub role: Role,
    pub content: Content,
    #[serde(default)]
    pub meta: serde_json::Value,
    /// Named-Abstraction references into patterns/ (Freedman 2026).
    #[serde(default)]
    pub refs: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    ClaudeCode,
    Cursor,
    Gemini,
    Aider,
    Slack,
    Telegram,
    Discord,
    CommanderEngine,
}

impl Source {
    /// Canonical short prefix used in filename `<src>_<id>.jsonl`.
    pub fn file_prefix(&self) -> &'static str {
        match self {
            Source::ClaudeCode => "cc",
            Source::Cursor => "cursor",
            Source::Gemini => "gemini",
            Source::Aider => "aider",
            Source::Slack => "slack",
            Source::Telegram => "telegram",
            Source::Discord => "discord",
            Source::CommanderEngine => "commander",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
    Tool,
}

/// Message body. `ToolRef` and `ImageRef` are content-addressed pointers
/// (spec §4.3 pointer substitution) to blobs stored under
/// `~/.mur/conversations/blob/<sha256>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Content {
    Text {
        #[serde(rename = "v")]
        value: String,
    },
    ToolRef {
        sha256: String,
        path: String,
        bytes: u64,
        #[serde(default)]
        desc: String,
    },
    ImageRef {
        sha256: String,
        path: String,
        #[serde(default)]
        desc: String,
    },
}

impl Content {
    /// Convenience constructor for `Content::Text`.
    pub fn text(s: impl Into<String>) -> Self {
        Content::Text { value: s.into() }
    }

    /// Plain-text projection for indexing/search. For pointer variants
    /// (`ToolRef`/`ImageRef`) returns the `desc` field, which may be an
    /// empty string if the producer didn't supply one — callers doing
    /// keyword search should treat empty results as "no indexable text"
    /// rather than treating them as matches.
    pub fn as_text(&self) -> &str {
        match self {
            Content::Text { value } => value,
            Content::ToolRef { desc, .. } | Content::ImageRef { desc, .. } => desc,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn message_text_roundtrip() {
        let m = Message {
            v: 1,
            ts: chrono::Utc
                .with_ymd_and_hms(2026, 4, 19, 11, 30, 45)
                .unwrap(),
            src: Source::ClaudeCode,
            conv: "3a8786a0".into(),
            role: Role::User,
            content: Content::text("hello"),
            meta: serde_json::json!({"project": "mur"}),
            refs: vec!["pattern:atomic-yaml-write".into()],
        };
        let line = serde_json::to_string(&m).unwrap();
        let back: Message = serde_json::from_str(&line).unwrap();
        assert_eq!(back.conv, "3a8786a0");
        assert!(matches!(back.content, Content::Text { ref value } if value == "hello"));
        assert!(matches!(back.src, Source::ClaudeCode));
        assert_eq!(back.refs, vec!["pattern:atomic-yaml-write".to_string()]);
    }

    #[test]
    fn message_tool_ref_roundtrip() {
        let m = Message {
            v: 1,
            ts: chrono::Utc
                .with_ymd_and_hms(2026, 4, 19, 11, 30, 45)
                .unwrap(),
            src: Source::ClaudeCode,
            conv: "x".into(),
            role: Role::Tool,
            content: Content::ToolRef {
                sha256: "abc".into(),
                path: "src/main.rs".into(),
                bytes: 1234,
                desc: "read main.rs".into(),
            },
            meta: serde_json::Value::Null,
            refs: vec![],
        };
        let line = serde_json::to_string(&m).unwrap();
        let back: Message = serde_json::from_str(&line).unwrap();
        assert!(matches!(back.content, Content::ToolRef { ref sha256, .. } if sha256 == "abc"));
    }

    #[test]
    fn message_image_ref_roundtrip() {
        let m = Message {
            v: 1,
            ts: chrono::Utc
                .with_ymd_and_hms(2026, 4, 19, 11, 30, 45)
                .unwrap(),
            src: Source::Cursor,
            conv: "x".into(),
            role: Role::Tool,
            content: Content::ImageRef {
                sha256: "def".into(),
                path: "attachments/diagram.png".into(),
                desc: "architecture diagram".into(),
            },
            meta: serde_json::Value::Null,
            refs: vec![],
        };
        let line = serde_json::to_string(&m).unwrap();
        assert!(line.contains("\"t\":\"image_ref\""));
        let back: Message = serde_json::from_str(&line).unwrap();
        assert!(matches!(back.content, Content::ImageRef { ref sha256, .. } if sha256 == "def"));
    }

    #[test]
    fn source_file_prefix_is_stable() {
        assert_eq!(Source::ClaudeCode.file_prefix(), "cc");
        assert_eq!(Source::Cursor.file_prefix(), "cursor");
        assert_eq!(Source::Gemini.file_prefix(), "gemini");
        assert_eq!(Source::Aider.file_prefix(), "aider");
        assert_eq!(Source::Slack.file_prefix(), "slack");
        assert_eq!(Source::Telegram.file_prefix(), "telegram");
        assert_eq!(Source::Discord.file_prefix(), "discord");
        assert_eq!(Source::CommanderEngine.file_prefix(), "commander");
    }

    #[test]
    fn message_deserializes_with_meta_and_refs_absent() {
        // Spec §12 forward-compat guarantee: older rows lacking meta/refs must
        // still deserialize. Both keys are completely absent from the input.
        let minimal = r#"{"v":1,"ts":"2026-04-19T11:30:45Z","src":"aider","conv":"c","role":"user","content":{"t":"text","v":"hi"}}"#;
        let m: Message = serde_json::from_str(minimal).unwrap();
        assert!(m.meta.is_null());
        assert!(m.refs.is_empty());
    }

    #[test]
    fn commander_turn_is_subset() {
        // Commander's existing ConversationTurn { timestamp: i64, role: String, text: String }
        // MUST deserialize when reshaped into the new Message format.
        let commander_json = r#"{"v":1,"ts":"2026-04-19T11:30:45Z","src":"slack","conv":"c","role":"user","content":{"t":"text","v":"hi"},"meta":{},"refs":[]}"#;
        let _m: Message = serde_json::from_str(commander_json).unwrap();
    }
}
