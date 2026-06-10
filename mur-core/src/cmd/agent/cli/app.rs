//! TUI application state and the pure (non-IO) state transitions.

use std::path::PathBuf;

use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders};
use tui_textarea::TextArea;

use super::markdown;
use super::persist::{Session, TurnRecord};
use super::stream::HitlRequest;

/// Spinner frames shown while the agent is generating.
pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Who authored a message in the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Agent,
    /// Local UI notice (slash-command output, errors, hints) — not persisted.
    System,
}

/// One message in the visible transcript.
#[derive(Debug, Clone)]
pub struct ChatMsg {
    pub role: Role,
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
}

impl ChatMsg {
    fn new(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            text: text.into(),
            thinking: String::new(),
            streaming: false,
            rendered: None,
        }
    }

    /// A finished agent message whose markdown is pre-rendered (resume path).
    fn agent_rendered(text: String) -> Self {
        let rendered = Some(markdown::render(&text).lines);
        Self {
            role: Role::Agent,
            text,
            thinking: String::new(),
            streaming: false,
            rendered,
        }
    }
}

/// A parsed slash command.
#[derive(Debug, PartialEq, Eq)]
pub enum SlashCmd {
    Help,
    Clear,
    Card,
    Sessions,
    Quit,
    Unknown(String),
}

/// Parse a leading-slash command. Returns `None` for ordinary chat input.
pub fn parse_slash(line: &str) -> Option<SlashCmd> {
    let line = line.trim();
    let rest = line.strip_prefix('/')?;
    let word = rest.split_whitespace().next().unwrap_or("");
    Some(match word {
        "help" | "h" | "?" => SlashCmd::Help,
        "clear" | "new" => SlashCmd::Clear,
        "card" => SlashCmd::Card,
        "sessions" | "ls" => SlashCmd::Sessions,
        "exit" | "quit" | "q" => SlashCmd::Quit,
        other => SlashCmd::Unknown(other.to_string()),
    })
}

/// The set of slash commands offered by tab-completion / `/help`.
pub const SLASH_COMMANDS: [&str; 6] = ["/help", "/clear", "/card", "/sessions", "/exit", "/quit"];

/// All mutable TUI state.
pub struct App {
    pub home: PathBuf,
    pub agent: String,
    pub messages: Vec<ChatMsg>,
    pub input: TextArea<'static>,
    pub context_task_id: Option<String>,
    pub current_task_id: Option<String>,
    pub streaming: bool,
    pub hitl: Option<HitlRequest>,
    pub session: Session,
    /// Lines scrolled up from the bottom (0 = pinned to newest).
    pub scroll_back: u16,
    pub spinner: usize,
    pub should_quit: bool,
    /// Set once we've warned the user that session writes are failing, so the
    /// warning isn't repeated every turn.
    persist_warned: bool,
}

impl App {
    pub fn new(home: PathBuf, agent: String, session: Session) -> Self {
        Self {
            home,
            agent,
            messages: Vec::new(),
            input: new_input(),
            context_task_id: None,
            current_task_id: None,
            streaming: false,
            hitl: None,
            session,
            scroll_back: 0,
            spinner: 0,
            should_quit: false,
            persist_warned: false,
        }
    }

    /// Append a turn to the session log, surfacing a write failure once.
    fn persist_turn(&mut self, role: &str, text: &str, task_id: Option<&str>) {
        if let Err(e) = self.session.append(role, text, task_id)
            && !self.persist_warned
        {
            self.persist_warned = true;
            self.push_system(format!("warning: session is not being saved: {e}"));
        }
    }

    /// Current input text (joined multiline).
    pub fn input_text(&self) -> String {
        self.input.lines().join("\n")
    }

    pub fn clear_input(&mut self) {
        self.input = new_input();
    }

    /// Replace the input buffer with `text` (used by slash-command completion).
    pub fn set_input(&mut self, text: &str) {
        self.input = new_input();
        self.input.insert_str(text);
    }

    pub fn push_system(&mut self, text: impl Into<String>) {
        self.messages.push(ChatMsg::new(Role::System, text));
        self.scroll_back = 0;
    }

    /// Record a user turn (visible + persisted) and return the new task id for
    /// the request.
    pub fn begin_user_turn(&mut self, text: &str) -> String {
        self.messages.push(ChatMsg::new(Role::User, text));
        self.persist_turn("user", text, None);
        // A fresh client-side task id per turn (used for cancellation).
        let task_id = uuid::Uuid::now_v7().to_string();
        self.current_task_id = Some(task_id.clone());
        self.streaming = true;
        self.scroll_back = 0;
        // Placeholder agent message that deltas accumulate into.
        let mut m = ChatMsg::new(Role::Agent, "");
        m.streaming = true;
        self.messages.push(m);
        task_id
    }

    pub fn append_delta(&mut self, text: &str, thinking: bool) {
        if let Some(m) = self.messages.last_mut()
            && m.role == Role::Agent
            && m.streaming
        {
            if thinking {
                m.thinking.push_str(text);
            } else {
                m.text.push_str(text);
            }
        }
        // NB: do not reset scroll_back here — that would yank the viewport back
        // to the bottom on every token, making it impossible to scroll up while
        // the agent streams. When the user hasn't scrolled (scroll_back == 0)
        // the render already stays pinned to the newest line as content grows.
    }

    /// Finalize the streaming agent turn with the authoritative reply. Persist
    /// and context-threading happen ONLY if a streaming agent message was
    /// matched, so a late event that no longer has a live turn can't write a
    /// phantom line or thread a stale context id.
    pub fn finish_agent_turn(&mut self, reply: String, task_id: Option<String>) {
        let mut body = None;
        if let Some(m) = self.messages.last_mut()
            && m.role == Role::Agent
            && m.streaming
        {
            if !reply.is_empty() {
                m.text = reply;
            }
            m.thinking.clear();
            m.streaming = false;
            m.rendered = Some(markdown::render(&m.text).lines);
            body = Some(m.text.clone());
        }
        if let Some(b) = body {
            if let Some(tid) = &task_id {
                self.context_task_id = Some(tid.clone());
            }
            self.persist_turn("agent", &b, task_id.as_deref());
        }
        self.streaming = false;
        self.current_task_id = None;
    }

    /// Mark a partial (cancelled) turn as finished without persisting a reply.
    pub fn finish_partial(&mut self) {
        if let Some(m) = self.messages.last_mut()
            && m.role == Role::Agent
            && m.streaming
        {
            m.thinking.clear();
            m.streaming = false;
            if m.text.is_empty() {
                m.text = "(cancelled)".to_string();
            }
        }
        self.streaming = false;
        self.current_task_id = None;
    }

    pub fn fail_turn(&mut self, err: &str) {
        if matches!(self.messages.last(), Some(m) if m.role == Role::Agent && m.streaming) {
            self.messages.pop();
        }
        self.push_system(format!("error: {err}"));
        self.streaming = false;
        self.current_task_id = None;
    }

    /// Reset to a brand-new conversation (drops server-side context). Any
    /// in-flight turn must already have been cancelled by the caller.
    pub fn start_new_session(&mut self, session: Session) {
        self.session = session;
        self.messages.clear();
        self.context_task_id = None;
        self.current_task_id = None;
        self.streaming = false;
        self.hitl = None;
        self.push_system("started a new conversation");
    }

    /// Load prior turns into the transcript (resume), threading the last agent
    /// task id as context.
    pub fn load_history(&mut self, turns: Vec<TurnRecord>) {
        let mut last_task = None;
        for t in turns {
            let role = match t.role.as_str() {
                "agent" => Role::Agent,
                _ => Role::User,
            };
            if role == Role::Agent {
                if let Some(id) = &t.task_id {
                    last_task = Some(id.clone());
                }
                self.messages.push(ChatMsg::agent_rendered(t.text));
            } else {
                self.messages.push(ChatMsg::new(role, t.text));
            }
        }
        self.context_task_id = last_task;
    }

    pub fn tick_spinner(&mut self) {
        self.spinner = (self.spinner + 1) % SPINNER.len();
    }
}

/// Build the styled multiline input widget.
fn new_input() -> TextArea<'static> {
    let mut ta = TextArea::default();
    ta.set_block(
        Block::default()
            .borders(Borders::ALL)
            .title(" message — Enter to send · Alt+Enter newline · /help · Ctrl+D quit "),
    );
    ta.set_cursor_line_style(Style::default());
    ta.set_placeholder_text("Type a message…");
    ta.set_placeholder_style(Style::default().fg(Color::DarkGray));
    ta
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn app() -> App {
        let home = tempdir().unwrap();
        let session = Session::create(home.path(), "a").unwrap();
        App::new(home.path().to_path_buf(), "a".into(), session)
    }

    #[test]
    fn parse_slash_variants() {
        assert_eq!(parse_slash("/help"), Some(SlashCmd::Help));
        assert_eq!(parse_slash("/new"), Some(SlashCmd::Clear));
        assert_eq!(parse_slash("/card"), Some(SlashCmd::Card));
        assert_eq!(parse_slash("/q"), Some(SlashCmd::Quit));
        assert_eq!(
            parse_slash("/bogus"),
            Some(SlashCmd::Unknown("bogus".into()))
        );
        assert_eq!(parse_slash("hello"), None);
        assert_eq!(parse_slash("  not/a/cmd"), None);
    }

    #[test]
    fn turn_lifecycle_threads_context() {
        let mut a = app();
        let tid = a.begin_user_turn("hi");
        assert!(a.streaming);
        assert_eq!(a.current_task_id.as_deref(), Some(tid.as_str()));
        a.append_delta("Hel", false);
        a.append_delta("lo", false);
        a.append_delta("(thinking)", true);
        assert_eq!(a.messages.last().unwrap().text, "Hello");
        a.finish_agent_turn("Hello!".into(), Some("server-task-1".into()));
        assert!(!a.streaming);
        assert_eq!(a.context_task_id.as_deref(), Some("server-task-1"));
        assert_eq!(a.messages.last().unwrap().text, "Hello!");
        assert!(!a.messages.last().unwrap().streaming);
    }

    #[test]
    fn late_finish_after_cancel_does_not_persist_or_thread() {
        // After Ctrl+C (finish_partial), a stale worker's Done must not rewrite
        // the bubble, persist a phantom line, or thread the cancelled task id.
        let mut a = app();
        a.begin_user_turn("hi");
        a.append_delta("partial", false);
        a.finish_partial();
        let ctx_before = a.context_task_id.clone();
        let len_before = a.messages.len();

        a.finish_agent_turn("the real answer".into(), Some("late-task".into()));

        assert_eq!(
            a.context_task_id, ctx_before,
            "stale task id must not be threaded"
        );
        assert_eq!(a.messages.len(), len_before, "no phantom message appended");
        assert_eq!(
            a.messages.last().unwrap().text,
            "partial",
            "bubble keeps partial text"
        );
    }

    #[test]
    fn append_delta_preserves_user_scroll() {
        let mut a = app();
        a.begin_user_turn("hi");
        a.scroll_back = 7;
        a.append_delta("tok", false);
        assert_eq!(
            a.scroll_back, 7,
            "streaming must not yank the viewport to bottom"
        );
    }

    #[test]
    fn finished_agent_turn_caches_markdown() {
        let mut a = app();
        a.begin_user_turn("hi");
        a.finish_agent_turn("**bold**".into(), Some("t1".into()));
        assert!(a.messages.last().unwrap().rendered.is_some());
    }

    #[test]
    fn fail_turn_drops_streaming_placeholder() {
        let mut a = app();
        a.begin_user_turn("hi");
        a.fail_turn("boom");
        // user msg + system error; the empty agent placeholder is removed.
        assert_eq!(a.messages.last().unwrap().role, Role::System);
        assert!(!a.streaming);
    }

    #[test]
    fn new_session_resets_context() {
        let mut a = app();
        a.begin_user_turn("hi");
        a.finish_agent_turn("ok".into(), Some("t1".into()));
        let s = Session::create(&a.home, &a.agent).unwrap();
        a.start_new_session(s);
        assert!(a.context_task_id.is_none());
        assert_eq!(a.messages.last().unwrap().role, Role::System);
    }
}
