//! A single tool-call step rendered inline in the cli transcript.

use std::time::Instant;

/// Lifecycle of a tool-call step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Running,
    Done,
    Error,
}

/// Max args lines shown inside an expanded card (mirrors the old HITL modal cap).
pub const ARGS_MAX_LINES: usize = 12;

/// One tool call, shown inline in the transcript.
#[derive(Debug, Clone)]
pub struct StepCard {
    pub id: String,
    pub name: String,
    pub args: serde_json::Value,
    pub state: StepState,
    pub output: String,
    pub truncated: bool,
    pub full_len: usize,
    pub error: Option<String>,
    pub started: Instant,
    pub duration_ms: Option<u64>,
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
            started: Instant::now(),
            duration_ms: None,
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
        self.state = if ok { StepState::Done } else { StepState::Error };
        self.output = output;
        self.truncated = truncated;
        self.full_len = full_len;
        self.error = error;
        self.duration_ms = Some(duration_ms);
    }

    pub fn glyph(&self) -> &'static str {
        match self.state {
            StepState::Running => "◐",
            StepState::Done => "✔",
            StepState::Error => "✗",
        }
    }

    /// One-line header summary: a compact hint of the first scalar arg, if any.
    pub fn summary(&self) -> String {
        let hint = self
            .args
            .as_object()
            .and_then(|m| m.values().find_map(|v| v.as_str()))
            .unwrap_or("");
        if hint.is_empty() {
            self.name.clone()
        } else {
            format!("{}  {}", self.name, hint)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{StepCard, StepState};

    fn card() -> StepCard {
        StepCard::new("s1".into(), "read".into(), serde_json::json!({ "path": "auth.rs" }))
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

    #[test]
    fn summary_is_one_line_name_plus_arg_hint() {
        let c = card();
        let s = c.summary();
        assert!(s.contains("read"));
        assert!(!s.contains('\n'));
    }
}
