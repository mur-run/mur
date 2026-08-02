//! A single tool-call step rendered inline in the cli transcript.

/// Lifecycle of a tool-call step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Running,
    Done,
    Error,
}

/// Max args lines shown inside an expanded card (mirrors the old HITL modal cap).
// Used by the step card UI renderer (render_card.rs).
pub const ARGS_MAX_LINES: usize = 12;

/// One tool call, shown inline in the transcript.
#[derive(Debug, Clone)]
pub struct StepCard {
    pub id: String,
    // name, args, started consumed by the step card UI renderer (render_card::card_lines).
    pub name: String,
    pub args: serde_json::Value,
    pub state: StepState,
    pub output: String,
    pub truncated: bool,
    pub full_len: usize,
    pub error: Option<String>,
    pub duration_ms: Option<u64>,
    /// True while this card's tool call is waiting on a HITL decision (P2 inline
    /// approval). Set when the matching `tool/approval_needed` arrives, cleared
    /// on decision.
    pub awaiting_hitl: bool,
    /// True when the card's tool call was auto-approved — by the `--auto-reads`
    /// read lane or by a session allow (`[a]`). Rendered as an `[auto]` tag on
    /// the header, which is the ONLY trace of auto-approval in the transcript:
    /// the separate notice row it replaced was printed for every call.
    pub auto_approved: bool,
}

impl StepCard {
    pub fn new(id: String, name: String, args: serde_json::Value) -> Self {
        Self {
            id,
            name,
            args,
            state: StepState::Running,
            output: String::new(),
            truncated: false,
            full_len: 0,
            error: None,
            duration_ms: None,
            awaiting_hitl: false,
            auto_approved: false,
        }
    }

    pub fn complete(
        &mut self,
        ok: bool,
        output: String,
        truncated: bool,
        full_len: usize,
        error: Option<String>,
        duration_ms: u64,
    ) {
        self.state = if ok {
            StepState::Done
        } else {
            StepState::Error
        };
        self.output = output;
        self.truncated = truncated;
        self.full_len = full_len;
        self.error = error;
        self.duration_ms = Some(duration_ms);
    }

    // glyph is called by render_card::card_lines.
    pub fn glyph(&self) -> &'static str {
        match self.state {
            StepState::Running => "◐",
            StepState::Done => "✔",
            StepState::Error => "✗",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{StepCard, StepState};

    fn card() -> StepCard {
        StepCard::new(
            "s1".into(),
            "read".into(),
            serde_json::json!({ "path": "auth.rs" }),
        )
    }

    #[test]
    fn new_card_is_running() {
        let c = card();
        assert_eq!(c.state, StepState::Running);
        assert_eq!(c.glyph(), "◐");
    }

    #[test]
    fn complete_ok_sets_done_and_glyph() {
        let mut c = card();
        c.complete(true, "412 lines".into(), false, 9, None, 8);
        assert_eq!(c.state, StepState::Done);
        assert_eq!(c.glyph(), "✔");
        assert_eq!(c.duration_ms, Some(8));
    }

    #[test]
    fn complete_err_sets_error_and_keeps_message() {
        let mut c = card();
        c.complete(false, "boom".into(), false, 4, Some("exit 1".into()), 3);
        assert_eq!(c.state, StepState::Error);
        assert_eq!(c.glyph(), "✗");
        assert_eq!(c.error.as_deref(), Some("exit 1"));
    }
}
