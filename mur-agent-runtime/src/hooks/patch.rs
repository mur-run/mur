//! Patch value types. Mutate hooks return one of these; `HookChain` folds
//! patches deterministically. Panic in handler #N drops only that patch;
//! prior patches are committed, the underlying view is never observed
//! half-mutated.

use serde::{Deserialize, Serialize};

/// Wrapper applied to untrusted content (B0 spotlighting).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UntrustedWrapper {
    pub tag: String,    // e.g. "untrusted_image_text"
    pub source: String, // e.g. "user_drop"
    pub content: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PromptPatch {
    /// Prefix prepended to the start of the system prompt.
    pub set_system_prefix: Option<String>,
    /// Suffix appended to the end of the system prompt.
    pub set_system_suffix: Option<String>,
    /// Untrusted wrappers to inject into the user message stream.
    pub wrap_untrusted: Vec<UntrustedWrapper>,
    /// Override sampling temperature.
    pub set_temperature: Option<f32>,
    /// Turn-flags (e.g., "after_untrusted_input") that `pre_tool_use` can read.
    pub turn_flags: Vec<String>,
}

impl PromptPatch {
    pub fn noop() -> Self {
        Self::default()
    }

    /// Deterministic fold. `other` runs after `self`.
    pub fn merge(mut self, other: PromptPatch) -> Self {
        self.set_system_prefix = match (self.set_system_prefix, other.set_system_prefix) {
            (Some(a), Some(b)) => Some(format!("{a}\n{b}")),
            (a, None) => a,
            (None, b) => b,
        };
        self.set_system_suffix = match (self.set_system_suffix, other.set_system_suffix) {
            (Some(a), Some(b)) => Some(format!("{a}\n{b}")),
            (a, None) => a,
            (None, b) => b,
        };
        self.wrap_untrusted.extend(other.wrap_untrusted);
        if let Some(t) = other.set_temperature {
            self.set_temperature = Some(t);
        }
        self.turn_flags.extend(other.turn_flags);
        self.turn_flags.sort();
        self.turn_flags.dedup();
        self
    }
}

/// Returned from `post_tool_use`. Default = pass-through (no mutation
/// of the `ToolResult` before persistence).
///
/// Currently the only field is `replace_output`, written by B0 rule 8
/// (PII redaction in `memory.*` tool results). Hooks that don't need
/// to mutate the output return `PostToolUsePatch::default()`.
///
/// Fold semantics in the chain runner: last-write-wins. If multiple
/// hooks return non-`None` `replace_output`, the last hook in chain
/// order takes effect. This keeps the dispatcher simple while M7.6
/// only has one redactor (B0SafetyHook); more elaborate merging can
/// land later if a second post-tool mutator ships.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PostToolUsePatch {
    /// Replace `ToolResult.output` with this value before persisting
    /// or returning to the model. `None` = no change.
    pub replace_output: Option<serde_json::Value>,
}

impl PostToolUsePatch {
    pub fn noop() -> Self {
        Self::default()
    }

    /// Deterministic fold. `other` runs after `self` (last-write-wins
    /// for `replace_output`).
    pub fn merge(mut self, other: PostToolUsePatch) -> Self {
        if let Some(o) = other.replace_output {
            self.replace_output = Some(o);
        }
        self
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessagePatch {
    /// Replace body (e.g., translation by i18n).
    pub set_body: Option<String>,
    /// Replace locale tag.
    pub set_locale: Option<String>,
    /// Drop the message entirely (e.g., linter rejects).
    pub drop: bool,
    /// Reason for drop (telemetry).
    pub drop_reason: Option<String>,
}

impl MessagePatch {
    pub fn noop() -> Self {
        Self::default()
    }

    pub fn drop_with(reason: &str) -> Self {
        Self {
            drop: true,
            drop_reason: Some(reason.into()),
            ..Self::default()
        }
    }

    /// Deterministic fold. `other` runs after `self`.
    pub fn merge(mut self, other: MessagePatch) -> Self {
        if let Some(b) = other.set_body {
            self.set_body = Some(b);
        }
        if let Some(l) = other.set_locale {
            self.set_locale = Some(l);
        }
        if other.drop {
            self.drop = true;
            self.drop_reason = other.drop_reason.or(self.drop_reason);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_patch_fold_is_deterministic() {
        let a = PromptPatch {
            set_system_prefix: Some("voice".into()),
            wrap_untrusted: vec![UntrustedWrapper {
                tag: "untrusted".into(),
                source: "img".into(),
                content: "x".into(),
            }],
            turn_flags: vec!["a".into()],
            ..PromptPatch::noop()
        };
        let b = PromptPatch {
            set_system_prefix: Some("safety".into()),
            turn_flags: vec!["b".into(), "a".into()],
            ..PromptPatch::noop()
        };
        let folded = a.clone().merge(b.clone());
        assert_eq!(folded.set_system_prefix.as_deref(), Some("voice\nsafety"));
        assert_eq!(folded.wrap_untrusted.len(), 1);
        assert_eq!(folded.turn_flags, vec!["a".to_string(), "b".to_string()]);

        // Idempotent under merge of `noop`.
        let again = folded.clone().merge(PromptPatch::noop());
        assert_eq!(again.set_system_prefix, folded.set_system_prefix);
    }

    #[test]
    fn message_patch_drop_short_circuits() {
        let a = MessagePatch::drop_with("linter");
        let b = MessagePatch {
            set_body: Some("ignored".into()),
            ..MessagePatch::noop()
        };
        let folded = a.merge(b);
        assert!(folded.drop);
        assert_eq!(folded.drop_reason.as_deref(), Some("linter"));
    }
}
