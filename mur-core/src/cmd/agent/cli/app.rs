//! TUI application state and the pure (non-IO) state transitions.

use std::collections::HashSet;
use std::path::PathBuf;

use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders};
use tui_textarea::TextArea;

use super::markdown;
use super::persist::{ChannelMeta, Session, TurnRecord};
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
    /// A `!command` the user ran locally, plus its output. Persisted, and
    /// queued so the agent sees it with the next message.
    Shell,
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
    /// `/channels [N]` — list channels or switch to channel N.
    Channels(Option<usize>),
    /// `/auto [on|off]` — toggle (None) or set session-wide auto-approval.
    Auto(Option<bool>),
    /// `/mcp [list|add|remove] …` — manage the agent's MCP servers.
    Mcp(Vec<String>),
    /// `/skill [list|add|remove] …` — manage the agent's skills.
    Skill(Vec<String>),
    Quit,
    Unknown(String),
}

/// Parse a leading-slash command. Returns `None` for ordinary chat input.
pub fn parse_slash(line: &str) -> Option<SlashCmd> {
    let line = line.trim();
    let rest = line.strip_prefix('/')?;
    let mut words = rest.split_whitespace();
    let word = words.next().unwrap_or("");
    Some(match word {
        "help" | "h" | "?" => SlashCmd::Help,
        "clear" | "new" => SlashCmd::Clear,
        "card" => SlashCmd::Card,
        "sessions" | "ls" => SlashCmd::Sessions,
        "channels" | "chan" => {
            SlashCmd::Channels(words.next().and_then(|s| s.parse::<usize>().ok()))
        }
        "auto" => SlashCmd::Auto(match words.next() {
            Some("on") => Some(true),
            Some("off") => Some(false),
            _ => None,
        }),
        "mcp" => SlashCmd::Mcp(words.map(str::to_string).collect()),
        "skill" | "skills" => SlashCmd::Skill(words.map(str::to_string).collect()),
        "exit" | "quit" | "q" => SlashCmd::Quit,
        other => SlashCmd::Unknown(other.to_string()),
    })
}

/// The set of slash commands offered by tab-completion / `/help`.
pub const SLASH_COMMANDS: [&str; 10] = [
    "/help",
    "/clear",
    "/card",
    "/sessions",
    "/channels",
    "/auto",
    "/mcp",
    "/skill",
    "/exit",
    "/quit",
];

#[allow(dead_code)]
pub const ESC_DOUBLE_WINDOW: std::time::Duration = std::time::Duration::from_millis(500);

#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq)]
pub enum EscAction {
    Arm,
    ClearInput,
    CancelAndRestore,
    Nothing,
}

/// Pure function — no wall-clock calls, fully testable.
#[allow(dead_code)]
pub fn esc_action(
    last_esc_at: Option<std::time::Instant>,
    streaming: bool,
    input_empty: bool,
) -> EscAction {
    if let Some(t) = last_esc_at
        && t.elapsed() < ESC_DOUBLE_WINDOW
    {
        return if streaming {
            EscAction::CancelAndRestore
        } else if !input_empty {
            EscAction::ClearInput
        } else {
            EscAction::Nothing
        };
    }
    // First press (or window expired)
    if streaming || !input_empty {
        EscAction::Arm
    } else {
        EscAction::Nothing
    }
}

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
    /// Cached live-channel id + state for status bar. Refreshed after each
    /// persisted turn on resume/switch. `None` until first append.
    pub channel: Option<ChannelMeta>,
    /// Lines scrolled up from the bottom (0 = pinned to newest).
    pub scroll_back: u16,
    pub spinner: usize,
    pub should_quit: bool,
    /// Session-wide auto-approval of every tool call (`/auto` or `--auto`).
    /// Never persisted: a new `mur agent cli` starts back at ask-first.
    pub auto_approve: bool,
    /// Tools the user marked "always allow" for THIS session via the HITL
    /// modal's `[a]` key. Same lifetime rules as `auto_approve`.
    pub session_tool_allow: HashSet<String>,
    /// `!command` blocks (command + output) not yet shown to the agent; they
    /// are prefixed onto the next user message so the agent has the context.
    pub pending_shell: Vec<String>,
    /// Set once we've warned the user that session writes are failing, so the
    /// warning isn't repeated every turn.
    persist_warned: bool,
    /// Working directory captured at CLI startup; sent to the agent once per
    /// session so it knows what project the user is in.
    pub cwd: Option<PathBuf>,
    /// True after CWD has been injected into the first outgoing message this
    /// session. Reset on `/clear` or channel switch so each new session
    /// re-establishes context.
    pub cwd_sent: bool,
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
            channel: None,
            scroll_back: 0,
            spinner: 0,
            should_quit: false,
            auto_approve: false,
            session_tool_allow: HashSet::new(),
            pending_shell: Vec::new(),
            persist_warned: false,
            cwd: std::env::current_dir().ok(),
            cwd_sent: false,
        }
    }

    /// The in-flight agent bubble, if any. Searched from the back instead of
    /// only checking `last()`: a system note pushed mid-turn (HITL "approved
    /// `tool`", a hint, a warning) lands AFTER the streaming bubble, and the
    /// turn's deltas/finish must still find their message (see #6: approving a
    /// tool call used to lose the whole reply).
    fn streaming_agent_mut(&mut self) -> Option<&mut ChatMsg> {
        self.messages
            .iter_mut()
            .rev()
            .find(|m| m.role == Role::Agent && m.streaming)
    }

    /// Append a turn to the session log, surfacing a write failure once.
    fn persist_turn(&mut self, role: &str, text: &str, task_id: Option<&str>) {
        match self.session.append(role, text, task_id) {
            Ok(()) => self.channel = self.session.current(),
            Err(e) => {
                if !self.persist_warned {
                    self.persist_warned = true;
                    self.push_system(format!("warning: session is not being saved: {e}"));
                }
            }
        }
    }

    /// Re-read live channel meta into the status-bar cache.
    pub fn refresh_channel(&mut self) {
        self.channel = self.session.current();
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
        if let Some(m) = self.streaming_agent_mut() {
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
        if let Some(m) = self.streaming_agent_mut() {
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
        if let Some(m) = self.streaming_agent_mut() {
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
        if let Some(i) = self
            .messages
            .iter()
            .rposition(|m| m.role == Role::Agent && m.streaming)
        {
            self.messages.remove(i);
        }
        self.push_system(format!("error: {err}"));
        self.streaming = false;
        self.current_task_id = None;
    }

    /// Reset to a brand-new conversation (drops server-side context). Any
    /// in-flight turn must already have been cancelled by the caller.
    pub fn start_new_session(&mut self, session: Session) {
        self.session = session;
        self.channel = None;
        self.messages.clear();
        self.context_task_id = None;
        self.current_task_id = None;
        self.streaming = false;
        self.hitl = None;
        self.cwd_sent = false;
        self.push_system("started a new conversation");
    }

    /// Switch live conversation to a channel by id: reopen its session, clear
    /// the transcript, rehydrate its turns, and refresh the status bar.
    pub fn switch_channel(&mut self, channel_id: &str) -> anyhow::Result<()> {
        let session = Session::open_existing(&self.home, &self.agent, channel_id)?;
        let turns = super::persist::load(&self.home, channel_id, &self.agent)?;
        self.session = session;
        self.channel = None;
        self.messages.clear();
        self.context_task_id = None;
        self.current_task_id = None;
        self.streaming = false;
        self.hitl = None;
        self.cwd_sent = false;
        self.load_history(turns);
        self.refresh_channel();
        Ok(())
    }

    /// Load prior turns into the transcript (resume), threading the last agent
    /// task id as context.
    /// Record a completed `!command` run: show it, persist it, and queue it
    /// for the agent's next turn.
    pub fn push_shell(&mut self, cmd: &str, output: &str) {
        let text = if output.is_empty() {
            format!("$ {cmd}")
        } else {
            format!("$ {cmd}\n{output}")
        };
        self.messages.push(ChatMsg::new(Role::Shell, text.clone()));
        self.persist_turn("shell", &text, None);
        self.pending_shell.push(text);
        self.scroll_back = 0;
    }

    /// Drain queued `!command` blocks into a context prefix for the next
    /// message, or `None` if there is nothing pending.
    pub fn take_pending_shell(&mut self) -> Option<String> {
        if self.pending_shell.is_empty() {
            return None;
        }
        let blocks = self.pending_shell.join("\n\n");
        self.pending_shell.clear();
        Some(format!(
            "[shell commands the user just ran locally in this chat]\n{blocks}\n[end of shell context]"
        ))
    }

    pub fn load_history(&mut self, turns: Vec<TurnRecord>) {
        let mut last_task = None;
        for t in turns {
            let role = match t.role.as_str() {
                "agent" => Role::Agent,
                "shell" => Role::Shell,
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

    /// Update the input textarea's border to reflect the current content mode.
    /// Call once per render frame so the style stays in sync without needing
    /// `&mut App` inside the draw closure.
    pub fn sync_input_block(&mut self) {
        let is_shell = self.input_text().trim_start().starts_with('!');
        let block = if is_shell {
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Red))
                .title(" ! shell command — output shared with agent ")
        } else {
            Block::default()
                .borders(Borders::ALL)
                .title(" message — Enter to send · Alt+Enter newline · /help · Ctrl+D quit ")
        };
        self.input.set_block(block);
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

    /// Helper that borrows an existing TempDir so the directory survives the test.
    fn app_at(home: &tempfile::TempDir) -> App {
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
    fn shell_blocks_queue_and_drain_into_prefix() {
        let mut a = app();
        a.push_shell("ls", "foo\nbar");
        a.push_shell("true", "");
        assert_eq!(
            a.messages.iter().filter(|m| m.role == Role::Shell).count(),
            2
        );
        let ctx = a.take_pending_shell().expect("pending blocks");
        assert!(ctx.contains("$ ls\nfoo\nbar"));
        assert!(ctx.contains("$ true"));
        assert!(a.take_pending_shell().is_none(), "drained");
    }

    #[test]
    fn finish_agent_turn_survives_mid_turn_system_note() {
        // Regression (#6): a system note pushed while streaming (e.g. HITL
        // "approved `bash`") lands after the agent bubble; the turn's finish
        // must still find that bubble instead of silently dropping the reply.
        let mut a = app();
        let tid = a.begin_user_turn("run ls");
        a.append_delta("partial", false);
        a.push_system("approved `bash`");
        a.append_delta(" more", false);
        a.finish_agent_turn("final reply".into(), Some(tid.clone()));
        let agent = a
            .messages
            .iter()
            .find(|m| m.role == Role::Agent)
            .expect("agent bubble");
        assert_eq!(agent.text, "final reply");
        assert!(!agent.streaming);
        assert_eq!(a.context_task_id.as_deref(), Some(tid.as_str()));
        assert!(!a.streaming);
    }

    #[test]
    fn fail_turn_drops_displaced_streaming_placeholder() {
        let mut a = app();
        a.begin_user_turn("hi");
        a.push_system("note lands after the placeholder");
        a.fail_turn("boom");
        assert!(
            !a.messages
                .iter()
                .any(|m| m.role == Role::Agent && m.streaming)
        );
        assert!(!a.streaming);
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

    #[test]
    fn parse_slash_channels() {
        assert_eq!(parse_slash("/channels"), Some(SlashCmd::Channels(None)));
        assert_eq!(
            parse_slash("/channels 2"),
            Some(SlashCmd::Channels(Some(2)))
        );
        assert_eq!(parse_slash("/chan"), Some(SlashCmd::Channels(None)));
        assert_eq!(parse_slash("/channels x"), Some(SlashCmd::Channels(None)));
    }

    #[test]
    fn switch_channel_loads_history_and_caches_meta() {
        let home = tempdir().unwrap();
        let mut a = app_at(&home);
        a.begin_user_turn("first question");
        a.finish_agent_turn("first answer".into(), Some("t1".into()));
        let first_id = a.channel.as_ref().expect("channel after turn").id.clone();

        // Start a new (second) session — channel should be cleared.
        let s = Session::create(&a.home, &a.agent).unwrap();
        a.start_new_session(s);
        assert!(a.channel.is_none(), "channel cleared after new session");

        // Switch back to the first channel.
        a.switch_channel(&first_id).unwrap();
        assert_eq!(a.channel.as_ref().unwrap().id, first_id);
        assert!(
            a.messages.iter().any(|m| m.text == "first question"),
            "history rehydrated after switch"
        );
    }
}

#[cfg(test)]
mod esc_action_tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn recent() -> Option<Instant> {
        Some(Instant::now() - Duration::from_millis(100))
    }

    fn expired() -> Option<Instant> {
        Some(Instant::now() - Duration::from_millis(600))
    }

    fn at_boundary() -> Option<Instant> {
        Some(Instant::now() - ESC_DOUBLE_WINDOW)
    }

    #[test]
    fn esc_arm_when_streaming_and_empty_input() {
        assert_eq!(esc_action(None, true, true), EscAction::Arm);
    }

    #[test]
    fn esc_arm_when_not_streaming_and_has_text() {
        assert_eq!(esc_action(None, false, false), EscAction::Arm);
    }

    #[test]
    fn esc_nothing_when_not_streaming_and_empty() {
        assert_eq!(esc_action(None, false, true), EscAction::Nothing);
    }

    #[test]
    fn esc_cancel_restore_on_second_press_while_streaming() {
        assert_eq!(esc_action(recent(), true, true), EscAction::CancelAndRestore);
    }

    #[test]
    fn esc_cancel_restore_on_second_press_streaming_has_text() {
        assert_eq!(esc_action(recent(), true, false), EscAction::CancelAndRestore);
    }

    #[test]
    fn esc_clear_input_on_second_press_not_streaming_has_text() {
        assert_eq!(esc_action(recent(), false, false), EscAction::ClearInput);
    }

    #[test]
    fn esc_nothing_on_second_press_not_streaming_empty() {
        assert_eq!(esc_action(recent(), false, true), EscAction::Nothing);
    }

    #[test]
    fn esc_arm_when_window_expired_streaming() {
        assert_eq!(esc_action(expired(), true, true), EscAction::Arm);
    }

    #[test]
    fn esc_arm_when_window_expired_has_text() {
        assert_eq!(esc_action(expired(), false, false), EscAction::Arm);
    }

    #[test]
    fn esc_arm_at_exact_boundary() {
        assert_eq!(esc_action(at_boundary(), false, false), EscAction::Arm);
    }

    #[test]
    fn esc_nothing_when_window_expired_not_streaming_empty() {
        assert_eq!(esc_action(expired(), false, true), EscAction::Nothing);
    }
}
