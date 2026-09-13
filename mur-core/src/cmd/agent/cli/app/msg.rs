//! Transcript message types, moved out of `app/mod.rs` for CLAUDE.md §4's 800-line rule.
//! Pure movement: every item below is verbatim.

use super::*;

/// Who authored a message in the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Agent,
    /// Local UI notice (slash-command output, errors, hints) — not persisted.
    System,
    /// A `!command` the user ran locally, plus its output. Persisted, and
    /// queued so the agent sees it with the next message.
    Shell,
}

/// Visual importance of a System notice, used to color-code the transcript so
/// errors/warnings stand out from ordinary hints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Severity {
    /// Ordinary hint or slash-command output.
    #[default]
    Info,
    /// Something the user should notice (cancelled, restored, degraded).
    Warn,
    /// A failure.
    Error,
    /// A completed action worth a nod (saved, done).
    Success,
}

/// One message in the visible transcript.
#[derive(Debug, Clone)]
pub struct ChatMsg {
    pub role: Role,
    /// Importance of a System notice; ignored for other roles.
    pub severity: Severity,
    /// Visible body. For a streaming agent turn this accumulates token deltas;
    /// it is replaced by the authoritative final reply on completion.
    pub text: String,
    /// Reasoning tokens accumulated while streaming (shown dimmed, then dropped).
    pub thinking: String,
    pub streaming: bool,
    /// Markdown rendered once when an agent turn finishes (or on resume), so the
    /// per-frame redraw never re-parses finished messages. `None` while
    /// streaming and for user/system messages.
    pub rendered: Option<Vec<Line<'static>>>,
    /// When set, this message renders a tool-call step card instead of text by
    /// role. `None` for ordinary user/agent/system/shell messages.
    pub step: Option<super::super::step::StepCard>,
    /// The turn's settlement card, lifted out of `text` so it can be drawn at
    /// the pane's real width every frame. `rendered` is cached width-free, so
    /// a card baked into it would keep a stale width across a resize.
    pub settlement: Option<String>,
}

impl ChatMsg {
    pub(super) fn new(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            severity: Severity::Info,
            text: text.into(),
            thinking: String::new(),
            streaming: false,
            rendered: None,
            step: None,
            settlement: None,
        }
    }

    /// A System notice tagged with an importance for color-coding.
    pub(super) fn system_sev(text: impl Into<String>, severity: Severity) -> Self {
        let mut m = Self::new(Role::System, text);
        m.severity = severity;
        m
    }

    /// A finished agent message whose markdown is pre-rendered (resume path).
    pub(super) fn agent_rendered(text: String, width: usize) -> Self {
        let (text, settlement) = super::super::settlement::split(&text);
        let rendered = Some(markdown::render(&text, width).lines);
        Self {
            role: Role::Agent,
            severity: Severity::Info,
            text,
            thinking: String::new(),
            streaming: false,
            rendered,
            step: None,
            settlement,
        }
    }

    /// A transcript entry that renders a tool-call step card.
    pub(super) fn tool(card: super::super::step::StepCard) -> Self {
        Self {
            role: Role::Agent,
            severity: Severity::Info,
            text: String::new(),
            thinking: String::new(),
            streaming: false,
            rendered: None,
            step: Some(card),
            settlement: None,
        }
    }
}

#[cfg(test)]
impl ChatMsg {
    pub fn for_test(role: Role, text: &str) -> Self {
        Self::new(role, text)
    }
    pub fn tool_for_test(card: super::super::step::StepCard) -> Self {
        Self::tool(card)
    }
}
