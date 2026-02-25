use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A fingerprint of a behavior observed in a session transcript.
///
/// Captures a distinct behavior (tool call pattern, command sequence, file edit pattern)
/// for cross-session emergence detection. Behaviors appearing across 3+ sessions
/// become candidates for automatic pattern creation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BehaviorFingerprint {
    /// Unique identifier (UUID)
    pub id: String,
    /// Which session this came from
    pub session_id: String,
    /// Short description of the behavior
    pub behavior: String,
    /// Extracted keywords for similarity matching
    pub keywords: Vec<String>,
    /// When this behavior was observed
    pub timestamp: DateTime<Utc>,
}

/// Events emitted by MUR Core for Commander and other consumers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MurEvent {
    PatternCreated {
        name: String,
    },
    PatternEvolved {
        name: String,
        old_importance: f64,
        new_importance: f64,
    },
    PatternDeprecated {
        name: String,
    },
    InjectionCompleted {
        patterns: Vec<String>,
        session_id: String,
    },
}

/// Conversation events (used by Commander watchers, defined here for sharing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConversationEvent {
    UserMessage {
        session_id: String,
        content: String,
        timestamp: i64,
    },
    AssistantMessage {
        session_id: String,
        content: String,
        timestamp: i64,
    },
    ToolCall {
        session_id: String,
        tool: String,
        args: serde_json::Value,
        result: Option<String>,
        timestamp: i64,
    },
    SessionStart {
        session_id: String,
        source: String,
    },
    SessionEnd {
        session_id: String,
    },
}
