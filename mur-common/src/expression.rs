//! Shared expression-change type used by both Hub and sidecar runtimes.
//!
//! The richer types (DwellSpec, ExpressionTrigger) live in `hub::trigger`
//! because they require YAML parsing. This module holds only the wire-safe
//! output type that the sidecar pushes to Hub over IPC.

use serde::{Deserialize, Serialize};

/// Describes the expression the pet should display, as emitted by the
/// ExpressionStateMachine.  Travels from mur-agent-runtime → Hub via IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpressionChange {
    /// Name of the expression (e.g. `"idle"`, `"smile"`, `"error"`).
    pub expression: String,
    /// Text to show in the speech bubble, if any.
    pub bubble_text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expression_change_roundtrip() {
        let ec = ExpressionChange {
            expression: "smile".into(),
            bubble_text: Some("Hello!".into()),
        };
        let json = serde_json::to_string(&ec).unwrap();
        let back: ExpressionChange = serde_json::from_str(&json).unwrap();
        assert_eq!(back.expression, "smile");
        assert_eq!(back.bubble_text.as_deref(), Some("Hello!"));
    }
}
