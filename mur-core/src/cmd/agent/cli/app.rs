//! TUI application state and the pure (non-IO) state transitions.

use std::collections::HashSet;
use std::io::IsTerminal;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Padding};
use tui_textarea::TextArea;

use super::complete::{Candidate, CompletionState};
use super::markdown;
use super::persist::{ChannelMeta, Session, TurnRecord};
use super::stream::HitlRequest;
use super::theme::Theme;
use super::welcome::{Blink, MascotMode, resolve_mascot_mode};

/// Spinner frames shown while the agent is generating.
pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Compact composer-border hint. Shift+Enter doesn't need an OS-specific
/// label (the key is "Shift" everywhere); the Alt/Option fallback chord only
/// shows up in the full hint below, where there's room to spell it out.
const ENTER_HINT_COMPACT: &str = " message — Enter · Shift+Enter · /help ";

/// Full composer-border hint. macOS calls the modifier "Option" even though
/// it's still crossterm's `ALT` — every other OS calls it "Alt".
#[cfg(target_os = "macos")]
const ENTER_HINT_FULL: &str = " message — Enter to send · Shift+Enter newline (Option+Enter also works) · Ctrl+V image · Ctrl+O transcript · /help · Ctrl+D quit";
#[cfg(not(target_os = "macos"))]
const ENTER_HINT_FULL: &str = " message — Enter to send · Shift+Enter newline (Alt+Enter also works) · Ctrl+V image · Ctrl+O transcript · /help · Ctrl+D quit";

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
    pub step: Option<super::step::StepCard>,
}

impl ChatMsg {
    fn new(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            severity: Severity::Info,
            text: text.into(),
            thinking: String::new(),
            streaming: false,
            rendered: None,
            step: None,
        }
    }

    /// A System notice tagged with an importance for color-coding.
    fn system_sev(text: impl Into<String>, severity: Severity) -> Self {
        let mut m = Self::new(Role::System, text);
        m.severity = severity;
        m
    }

    /// A finished agent message whose markdown is pre-rendered (resume path).
    fn agent_rendered(text: String) -> Self {
        let rendered = Some(markdown::render(&text).lines);
        Self {
            role: Role::Agent,
            severity: Severity::Info,
            text,
            thinking: String::new(),
            streaming: false,
            rendered,
            step: None,
        }
    }

    /// A transcript entry that renders a tool-call step card.
    fn tool(card: super::step::StepCard) -> Self {
        Self {
            role: Role::Agent,
            severity: Severity::Info,
            text: String::new(),
            thinking: String::new(),
            streaming: false,
            rendered: None,
            step: Some(card),
        }
    }
}

#[cfg(test)]
impl ChatMsg {
    pub fn for_test(role: Role, text: &str) -> Self {
        Self::new(role, text)
    }
    pub fn tool_for_test(card: super::step::StepCard) -> Self {
        Self::tool(card)
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
    /// `/verbose [on|off]` — toggle (None) or set expanded tool-card rendering.
    Verbose(Option<bool>),
    /// `/mcp [list|add|remove] …` — manage the agent's MCP servers.
    Mcp(Vec<String>),
    /// `/skill [list|add|remove] …` — manage the agent's skills.
    Skill(Vec<String>),
    /// `/skin [dark|light|mur]` — show or switch the active skin (persists to config).
    Skin(Option<String>),
    /// `/panel [tab] [target]` — open/drive the MUR Hub companion window.
    Panel(Vec<String>),
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
        "verbose" => SlashCmd::Verbose(match words.next() {
            Some("on") => Some(true),
            Some("off") => Some(false),
            _ => None,
        }),
        "mcp" => SlashCmd::Mcp(words.map(str::to_string).collect()),
        "skill" | "skills" => SlashCmd::Skill(words.map(str::to_string).collect()),
        "skin" | "theme" => SlashCmd::Skin(words.next().map(str::to_string)),
        "panel" => SlashCmd::Panel(words.map(str::to_string).collect()),
        "exit" | "quit" | "q" => SlashCmd::Quit,
        other => SlashCmd::Unknown(other.to_string()),
    })
}

pub const ESC_DOUBLE_WINDOW: std::time::Duration = std::time::Duration::from_millis(500);

#[derive(Debug, PartialEq, Eq)]
pub enum EscAction {
    Arm,
    ClearInput,
    CancelAndRestore,
    Nothing,
}

/// Pure function — no wall-clock calls, fully testable.
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

/// Result of a keypress while the transcript overlay (Ctrl+O) is open.
#[derive(Debug, PartialEq, Eq)]
pub enum OverlayKeyAction {
    /// Close the overlay and resume normal input.
    Close,
    /// Close the overlay, then request the app quit (Ctrl+D while reading).
    CloseAndQuit,
    /// Swallow the key — never inserted into the composer.
    Ignore,
}

/// Pure function — no IO, fully testable. The transcript overlay only
/// recognises Esc/Enter (return to chat) and Ctrl+D (quit); every other key
/// is swallowed so it can never leak into the input box once the overlay
/// closes.
pub fn overlay_key_action(code: KeyCode, modifiers: KeyModifiers) -> OverlayKeyAction {
    if code == KeyCode::Char('d') && modifiers.contains(KeyModifiers::CONTROL) {
        return OverlayKeyAction::CloseAndQuit;
    }
    match code {
        KeyCode::Esc | KeyCode::Enter => OverlayKeyAction::Close,
        _ => OverlayKeyAction::Ignore,
    }
}

/// The terminal surface the TUI is currently drawing on.
///
/// `Inline` is the steady state: a small, FIXED-height inline viewport (the
/// composer + status + the currently-streaming reply's tail) sitting on the
/// main screen. Every message that finishes gets flushed straight into the
/// terminal's own native scrollback via `Terminal::insert_before` — so mouse
/// wheel scroll and text selection over history are 100% native, no app
/// mouse-capture needed. The viewport height is a compile-time constant and
/// is NEVER resized after creation: `Terminal::resize` on an Inline viewport
/// queries the terminal for its cursor position, and that query races the
/// async `EventStream` reader for the same stdin bytes and hangs under
/// nested tmux/remote terminals — fixing the height instead of resizing it
/// sidesteps that entirely.
///
/// `Fullscreen` is for heavy overlays (Ctrl+O transcript, `/mcp`/`/skill`
/// browsers) that need the whole screen: entering/leaving the alternate
/// screen is a fire-and-forget mode-set escape code, not a query, so it's
/// safe to toggle on every mode edge (see `sync_surface`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    Inline,
    Fullscreen,
}

/// All mutable TUI state.
pub struct App {
    pub home: PathBuf,
    pub agent: String,
    pub messages: Vec<ChatMsg>,
    pub input: TextArea<'static>,
    pub context_task_id: Option<String>,
    pub current_task_id: Option<String>,
    /// Params of the in-flight `message/send`, kept so a dial that dies
    /// before the turn starts can be replayed once (the user's message is
    /// already persisted to the channel by then). Refreshed on each (re)send.
    pub inflight_params: Option<serde_json::Value>,
    /// True once the current turn's send has been replayed (one retry max).
    pub send_retried: bool,
    /// True once the runtime produced anything for the current turn (delta,
    /// step, or HITL event) — after that a failed dial is never replayed,
    /// since the agent may already have done side-effectful work.
    pub turn_produced_output: bool,
    pub streaming: bool,
    pub hitl: Option<HitlRequest>,
    pub session: Session,
    /// Cached live-channel id + state for status bar. Refreshed after each
    /// persisted turn on resume/switch. `None` until first append.
    pub channel: Option<ChannelMeta>,
    /// Lines scrolled up from the bottom (0 = pinned to newest).
    pub scroll_back: u16,
    /// Transcript viewport height (rows), captured each render so PageUp/Down
    /// move a screenful and `scroll_back` can be clamped to the real maximum.
    pub scroll_page: u16,
    /// User adjustment to the chooser band height (Ctrl+↑/↓ while the
    /// chooser is open), in rows relative to the auto-computed height.
    /// Persists for the session so a preferred size sticks between turns.
    pub chooser_grow: i16,
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
    /// Panel server handle (`/panel` companion window). None only in tests;
    /// dropping it removes the session record + socket.
    pub panel: Option<super::panel::PanelHandle>,
    /// Per-session gate for forwarding agent-output deltas to the Panel.
    /// Default OFF; toggled ONLY via `/panel stream on|off` from this
    /// terminal — the Hub has no frame that can flip it (fail-closed).
    pub panel_stream: bool,
    /// Working directory captured at CLI startup; sent to the agent once per
    /// session so it knows what project the user is in.
    pub cwd: Option<PathBuf>,
    /// True after CWD has been injected into the first outgoing message this
    /// session. Reset on `/clear` or channel switch so each new session
    /// re-establishes context.
    pub cwd_sent: bool,
    /// Active visual skin, resolved at startup. Updated live by `/skin`.
    pub theme: &'static Theme,
    pub last_esc_at: Option<std::time::Instant>,
    pub esc_hint: bool,
    /// Forces a full ratatui repaint on the next frame after we manipulate the
    /// raw terminal outside the Terminal object (e.g. Ctrl+O scrollback dump).
    pub needs_full_redraw: bool,
    /// Current render surface — see `RenderMode`.
    pub render_mode: RenderMode,
    /// Count of leading `messages` already flushed into the terminal's native
    /// scrollback via `Terminal::insert_before` (Inline mode). Only messages
    /// at index `>= flushed_upto` are still painted in the live viewport —
    /// in practice that's at most the one currently-streaming message, since
    /// a message is flushed the moment it stops streaming.
    pub flushed_upto: usize,
    /// True while the Ctrl+O transcript overlay is showing. The overlay
    /// stays in raw mode/alt-screen and keys route through the normal event
    /// loop (`overlay_key_action`) instead of a blocking stdin read.
    pub overlay_open: bool,
    /// Plain-text transcript rendered full-screen while `overlay_open` is
    /// true. `None` when the overlay is closed.
    pub overlay_text: Option<String>,
    /// Armed-at timestamp for the Ctrl+C two-press-to-quit confirmation when
    /// the composer is empty and idle. Mirrors `last_esc_at`.
    pub last_ctrl_c_at: Option<std::time::Instant>,
    pub ctrl_c_hint: bool,
    pub last_sent: Option<String>,
    /// `(mime, base64)` of an image staged for the next message — either a
    /// clipboard screenshot (Ctrl+V) or an image file the terminal pasted as a
    /// path (Cmd+V / drag-drop). Sent as an inline image part, cleared on send.
    pub pending_image: Option<(String, String)>,
    /// Mascot blink driver for the startup welcome screen. Render is a pure
    /// function of elapsed time; the event loop wakes on its next deadline.
    pub blink: Blink,
    /// Mascot color/animation mode, resolved once at startup from the theme
    /// and terminal capabilities (NO_COLOR / non-TTY / TERM=dumb → static).
    pub mascot_mode: MascotMode,
    /// True while the terminal is focused. Driven by crossterm focus events;
    /// used to suppress notifications while the user is watching.
    pub focused: bool,
    /// Wall-clock instant when the current agent turn began (set in
    /// `begin_user_turn`, cleared in `finish_agent_turn` / `fail_turn`).
    pub turn_started: Option<std::time::Instant>,
    /// Cumulative token counts for this session (all turns combined).
    pub session_in: u64,
    pub session_out: u64,
    /// Token counts for the most recent completed turn.
    pub turn_in: u64,
    pub turn_out: u64,
    /// Last-known context fill from the runtime's `Task.usage.context_tokens`.
    pub ctx_tokens: u64,
    /// Agent's model pricing loaded at startup (used by the footer renderer).
    pub pricing: super::footer::Pricing,
    /// Set to `true` when `StepStarted` fires this turn; used by the footer
    /// to distinguish "pure chat" from "agentic" turns.
    pub saw_step_this_turn: bool,
    /// A HITL approval arrived this turn (any runtime). Paired with
    /// `saw_step_this_turn` to detect an old runtime that ran a tool but
    /// streamed no step events.
    pub saw_hitl_this_turn: bool,
    /// The "restart for step view" hint has been shown once this session.
    pub step_hint_shown: bool,
    /// Optional per-session cost ceiling in USD (`--budget-usd`). `None` = no
    /// limit. Task 2 gates new turns when `session_cost() >= budget_usd`.
    pub budget_usd: Option<f64>,
    /// Auto-approve read-only bash commands for this session (`--auto-reads`).
    /// Opt-in, off by default. The classifier is conservative (fail-safe false
    /// on anything uncertain). Every auto-approval is tagged on the step card.
    pub auto_reads: bool,
    /// When true, tool-call step cards render fully (args + result) instead of
    /// the default one-line collapsed summary. Toggled with `/verbose`.
    pub cards_expanded: bool,
    /// Live completion menu (slash commands / agent skills). `None` = closed.
    /// Derived from the input text — recomputed on every edit by `mod.rs`.
    pub completion: Option<CompletionState>,
    /// This agent's skills as menu candidates, loaded once at startup.
    pub skills: Vec<Candidate>,
    /// Replies captured from a `suggest_replies` tool call this turn, revealed
    /// after the turn finishes (see `reveal_suggestions`).
    pub pending_suggestions: Vec<super::suggest::Suggestion>,
    /// The single suggestion currently shown as ghost placeholder text, if any.
    pub suggestion_ghost: Option<String>,
    /// Set when the visible transcript no longer matches the conversation
    /// (/clear, /channels switch): the event loop wipes screen + scrollback
    /// and re-anchors a fresh viewport before the next draw.
    pub wants_screen_wipe: bool,
    /// Sent-message history for shell-style ↑/↓ recall in the composer.
    pub sent_history: Vec<String>,
    /// Current position while browsing `sent_history` (None = not browsing).
    pub hist_idx: Option<usize>,
    /// Draft stashed when browsing starts; restored on ↓ past the newest.
    pub hist_stash: String,
    /// Input-driven suggestions (spec §3.5): last input text observed by the
    /// debounce, last snapshot actually sent, and the pending deadline.
    pub panel_input_seen: String,
    pub panel_input_sent: String,
    pub panel_input_deadline: Option<std::time::Instant>,
}

impl App {
    pub fn new(home: PathBuf, agent: String, session: Session, theme: &'static Theme) -> Self {
        Self {
            home,
            agent,
            messages: Vec::new(),
            input: new_input(),
            context_task_id: None,
            current_task_id: None,
            inflight_params: None,
            send_retried: false,
            turn_produced_output: false,
            streaming: false,
            hitl: None,
            session,
            channel: None,
            scroll_back: 0,
            scroll_page: 0,
            chooser_grow: 0,
            spinner: 0,
            should_quit: false,
            auto_approve: false,
            session_tool_allow: HashSet::new(),
            pending_shell: Vec::new(),
            persist_warned: false,
            panel: None,
            panel_stream: false,
            cwd: std::env::current_dir().ok(),
            cwd_sent: false,
            theme,
            last_esc_at: None,
            esc_hint: false,
            needs_full_redraw: false,
            render_mode: RenderMode::Inline,
            flushed_upto: 0,
            overlay_open: false,
            overlay_text: None,
            last_ctrl_c_at: None,
            ctrl_c_hint: false,
            last_sent: None,
            pending_image: None,
            blink: Blink::new(),
            // Resolve color/animation once: env + TTY don't change mid-session.
            mascot_mode: resolve_mascot_mode(theme, std::io::stdout().is_terminal()),
            // Assume focused at startup; crossterm corrects it on the first
            // FocusLost. (Terminals that don't report focus stay `true` → no
            // notifications, which is the safe default.)
            focused: true,
            turn_started: None,
            session_in: 0,
            session_out: 0,
            turn_in: 0,
            turn_out: 0,
            ctx_tokens: 0,
            pricing: super::footer::Pricing::default(),
            saw_step_this_turn: false,
            saw_hitl_this_turn: false,
            step_hint_shown: false,
            budget_usd: None,
            auto_reads: false,
            cards_expanded: false,
            completion: None,
            skills: Vec::new(),
            pending_suggestions: Vec::new(),
            suggestion_ghost: None,
            wants_screen_wipe: false,
            sent_history: Vec::new(),
            hist_idx: None,
            hist_stash: String::new(),
            panel_input_seen: String::new(),
            panel_input_sent: String::new(),
            panel_input_deadline: None,
        }
    }

    /// Estimated cumulative session cost in USD, or `None` if the model's
    /// pricing is unknown. Used by Task 2 to gate new turns against
    /// `budget_usd`. Fail-open: `None` means "can't price it, allow the turn".
    pub fn session_cost(&self) -> Option<f64> {
        super::footer::turn_cost(
            &self.pricing,
            &super::footer::UsageCounts {
                input: self.session_in,
                output: self.session_out,
            },
        )
    }

    /// True when a USD cap is set and the estimated session spend has reached
    /// it. Fails OPEN: an unpriced model (`session_cost() == None`) never blocks.
    pub fn over_budget(&self) -> bool {
        match (self.budget_usd, self.session_cost()) {
            (Some(cap), Some(spent)) => spent >= cap,
            _ => false,
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

    /// Ingest a `Task.usage` JSON object: update per-turn and session counters
    /// and refresh `ctx_tokens` if the runtime emitted `context_tokens`.
    pub fn apply_usage(&mut self, usage: &serde_json::Value) {
        let u = super::footer::parse_usage(usage);
        self.turn_in = u.input;
        self.turn_out = u.output;
        self.session_in += u.input;
        self.session_out += u.output;
        if let Some(c) = super::footer::context_tokens(usage) {
            self.ctx_tokens = c;
        }
    }

    /// Reveal suggestions captured this turn: one → ghost placeholder, many →
    /// completion overlay. No-op unless the composer is empty. Clears
    /// `pending_suggestions` either way.
    pub fn reveal_suggestions(&mut self) {
        let pending = std::mem::take(&mut self.pending_suggestions);
        let input_empty = self.input_text().is_empty();
        match super::suggest::plan_reveal(pending, input_empty) {
            super::suggest::Reveal::None => {}
            super::suggest::Reveal::Ghost(text) => {
                self.suggestion_ghost = Some(text.clone());
                self.input.set_placeholder_text(text);
            }
            super::suggest::Reveal::Chooser(items) => {
                let candidates: Vec<super::complete::Candidate> = items
                    .into_iter()
                    .map(|s| super::complete::Candidate {
                        display: s.text.clone(),
                        insert: s.text,
                        desc: s.desc.unwrap_or_default(),
                        has_children: false,
                    })
                    .collect();
                self.completion = Some(super::complete::CompletionState {
                    items: candidates,
                    selected: 0,
                    spaced: true,
                });
            }
        }
    }

    /// Clear the ghost placeholder (used when the user starts typing).
    pub fn clear_suggestion_ghost(&mut self) {
        if self.suggestion_ghost.take().is_some() {
            self.input.set_placeholder_text("Type a message…");
        }
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

    /// System notice tagged with an importance so the transcript can color-code
    /// it (errors red, warnings amber, successes green).
    pub fn push_system_sev(&mut self, text: impl Into<String>, severity: Severity) {
        self.messages.push(ChatMsg::system_sev(text, severity));
        self.scroll_back = 0;
    }

    pub fn push_error(&mut self, text: impl Into<String>) {
        self.push_system_sev(text, Severity::Error);
    }

    pub fn push_warn(&mut self, text: impl Into<String>) {
        self.push_system_sev(text, Severity::Warn);
    }

    pub fn push_success(&mut self, text: impl Into<String>) {
        self.push_system_sev(text, Severity::Success);
    }

    /// Record a user turn (visible + persisted) and return the new task id for
    /// the request.
    pub fn begin_user_turn(&mut self, text: &str) -> String {
        self.messages.push(ChatMsg::new(Role::User, text));
        self.persist_turn("user", text, None);
        // A fresh client-side task id per turn (used for cancellation).
        let task_id = uuid::Uuid::now_v7().to_string();
        self.current_task_id = Some(task_id.clone());
        self.inflight_params = None;
        self.send_retried = false;
        self.turn_produced_output = false;
        self.streaming = true;
        self.turn_started = Some(std::time::Instant::now());
        self.turn_in = 0;
        self.turn_out = 0;
        self.saw_step_this_turn = false;
        self.saw_hitl_this_turn = false;
        self.pending_suggestions.clear();
        self.scroll_back = 0;
        // Placeholder agent message that deltas accumulate into.
        let mut m = ChatMsg::new(Role::Agent, "");
        m.streaming = true;
        self.messages.push(m);
        task_id
    }

    pub fn append_delta(&mut self, text: &str, thinking: bool) {
        if self.streaming_agent_mut().is_none() {
            // Prior segment was frozen by a step card; start a new one.
            let mut m = ChatMsg::new(Role::Agent, "");
            m.streaming = true;
            self.messages.push(m);
        }
        if let Some(m) = self.streaming_agent_mut() {
            if thinking {
                m.thinking.push_str(text);
            } else {
                m.text.push_str(text);
            }
        }
        if !thinking
            && self.panel_stream
            && let Some(panel) = &self.panel
        {
            panel.send(mur_common::panel::PanelFrame::Stream {
                delta: text.to_string(),
            });
        }
        // NB: do not reset scroll_back here — that would yank the viewport back
        // to the bottom on every token, making it impossible to scroll up while
        // the agent streams. When the user hasn't scrolled (scroll_back == 0)
        // the render already stays pinned to the newest line as content grows.
    }

    /// If a tool needed approval this turn but no step events arrived, the agent
    /// is running an old runtime that predates the Glass Box step stream. Nudge
    /// the user to restart it — once per session.
    pub fn maybe_step_hint(&mut self) {
        if self.saw_hitl_this_turn && !self.saw_step_this_turn && !self.step_hint_shown {
            self.step_hint_shown = true;
            let agent = self.agent.clone();
            self.push_system(format!(
                "↻ this agent ran a tool without streaming step detail — restart it (mur agent restart {agent}) for the step view"
            ));
        }
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
            m.streaming = false;
            m.rendered = Some(markdown::render(&m.text).lines);
            body = Some(m.text.clone());
        } else if self.streaming && !reply.is_empty() {
            // Tool-using turns run the agentic loop, which doesn't stream text
            // deltas — the empty placeholder was dropped when the first step card
            // arrived, so there's no trailing segment. Push the final reply as its
            // own finished message instead of dropping it.
            // Guard: self.streaming is false after finish_partial() so stale
            // Done events from cancelled tasks are still silently ignored.
            self.messages.push(ChatMsg::agent_rendered(reply.clone()));
            self.scroll_back = 0;
            body = Some(reply);
        }
        if let Some(b) = body {
            if let Some(tid) = &task_id {
                self.context_task_id = Some(tid.clone());
            }
            self.persist_turn("agent", &b, task_id.as_deref());
        }
        self.streaming = false;
        self.current_task_id = None;
        self.turn_started = None;
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
        self.turn_started = None;
    }

    /// Freeze the current streaming text segment (or drop it if empty) and push
    /// a new running tool-call card.
    pub fn push_step_started(&mut self, step_id: String, name: String, args: serde_json::Value) {
        // Find the streaming agent segment, if any.
        let idx = self
            .messages
            .iter()
            .rposition(|m| m.role == Role::Agent && m.streaming);
        if let Some(i) = idx {
            let is_empty = self.messages[i].text.is_empty() && self.messages[i].thinking.is_empty();
            if is_empty {
                // Empty placeholder (agent called a tool before any text) — drop it.
                self.messages.remove(i);
            } else {
                // Freeze the current text segment.
                let rendered = Some(markdown::render(&self.messages[i].text).lines);
                self.messages[i].streaming = false;
                self.messages[i].rendered = rendered;
            }
        }
        self.messages.push(ChatMsg::tool(super::step::StepCard::new(
            step_id, name, args,
        )));
        self.scroll_back = 0;
    }

    /// Mark the matching step card as completed.
    #[allow(clippy::too_many_arguments)]
    pub fn update_step_completed(
        &mut self,
        step_id: &str,
        ok: bool,
        output: String,
        truncated: bool,
        full_len: usize,
        error: Option<String>,
        duration_ms: u64,
    ) {
        if let Some(card) = self
            .messages
            .iter_mut()
            .rev()
            .find_map(|m| m.step.as_mut().filter(|c| c.id == step_id))
        {
            card.complete(ok, output, truncated, full_len, error, duration_ms);
        }
    }

    /// Flag the card with this `step_id` as awaiting a HITL decision.
    pub fn mark_card_awaiting(&mut self, step_id: &str) {
        if let Some(card) = self
            .messages
            .iter_mut()
            .rev()
            .find_map(|m| m.step.as_mut().filter(|c| c.id == step_id))
        {
            card.awaiting_hitl = true;
        }
    }

    /// Mark the card with this `step_id` as auto-approved by the read lane
    /// (`--auto-reads`). Call BEFORE moving `req` into `app.hitl`.
    pub fn mark_card_auto_approved(&mut self, step_id: &str) {
        if let Some(card) = self
            .messages
            .iter_mut()
            .rev()
            .find_map(|m| m.step.as_mut().filter(|c| c.id == step_id))
        {
            card.auto_approved = true;
        }
    }

    /// Clear the awaiting-HITL flag on the card with this `step_id`.
    pub fn clear_card_awaiting(&mut self, step_id: &str) {
        if let Some(card) = self
            .messages
            .iter_mut()
            .rev()
            .find_map(|m| m.step.as_mut().filter(|c| c.id == step_id))
        {
            card.awaiting_hitl = false;
        }
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
        self.turn_started = None;
    }

    /// Drop the binding to a turn that no longer exists on the runtime (it
    /// restarted; tasks live in memory only). Removes an empty streaming
    /// placeholder, freezes any partial text already streamed, and clears the
    /// in-flight state so the next input starts a fresh `message/send`
    /// instead of steering a dead task.
    pub fn drop_dead_turn(&mut self) {
        if let Some(i) = self
            .messages
            .iter()
            .rposition(|m| m.role == Role::Agent && m.streaming)
        {
            if self.messages[i].text.is_empty() {
                self.messages.remove(i);
            } else {
                self.messages[i].streaming = false;
            }
        }
        self.streaming = false;
        self.current_task_id = None;
        self.turn_started = None;
        self.inflight_params = None;
    }

    /// Surface — and persist into the channel — that the last user message
    /// never reached the runtime. The human event is already durably in the
    /// channel by send time, so without this marker the history would claim
    /// the agent saw a message that never became a task.
    pub fn mark_undelivered(&mut self) {
        self.push_error(
            "message NOT delivered — the agent never received it; \
             check the agent is running, then resend",
        );
        self.persist_turn(
            "shell",
            "[message not delivered: the agent runtime never received the message above]",
            None,
        );
    }

    /// Reset to a brand-new conversation (drops server-side context). Any
    /// in-flight turn must already have been cancelled by the caller.
    pub fn start_new_session(&mut self, session: Session) {
        self.session = session;
        self.channel = None;
        self.messages.clear();
        self.flushed_upto = 0;
        self.needs_full_redraw = true;
        self.context_task_id = None;
        self.current_task_id = None;
        self.streaming = false;
        self.hitl = None;
        self.cwd_sent = false;
        self.last_sent = None;
        self.last_esc_at = None;
        self.esc_hint = false;
        self.last_ctrl_c_at = None;
        self.ctrl_c_hint = false;
        self.wants_screen_wipe = true;
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
        self.flushed_upto = 0;
        self.context_task_id = None;
        self.current_task_id = None;
        self.streaming = false;
        self.hitl = None;
        self.cwd_sent = false;
        self.wants_screen_wipe = true;
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
                if role == Role::User {
                    self.history_record(&t.text);
                }
                self.messages.push(ChatMsg::new(role, t.text));
            }
        }
        self.context_task_id = last_task;
    }

    // ── Composer input history (shell-style ↑/↓ recall) ────────────────────

    /// Record a sent message for ↑ recall. Skips blanks and immediate
    /// duplicates; always exits browsing mode.
    pub fn history_record(&mut self, text: &str) {
        if !text.trim().is_empty() && self.sent_history.last().map(String::as_str) != Some(text) {
            self.sent_history.push(text.to_string());
        }
        self.hist_idx = None;
        self.hist_stash.clear();
    }

    /// ↑ on the composer's first line: recall the previous sent message.
    /// Returns false (key not consumed) when there is no history.
    pub fn history_prev(&mut self) -> bool {
        if self.sent_history.is_empty() {
            return false;
        }
        let next = match self.hist_idx {
            None => {
                self.hist_stash = self.input_text();
                self.sent_history.len() - 1
            }
            Some(0) => return true, // already at oldest — swallow the key
            Some(i) => i - 1,
        };
        self.hist_idx = Some(next);
        let text = self.sent_history[next].clone();
        self.set_input(&text);
        true
    }

    /// ↓ on the composer's last line while browsing: newer entry, or restore
    /// the stashed draft past the newest. Returns false when not browsing.
    pub fn history_next(&mut self) -> bool {
        let Some(i) = self.hist_idx else {
            return false;
        };
        if i + 1 < self.sent_history.len() {
            self.hist_idx = Some(i + 1);
            let text = self.sent_history[i + 1].clone();
            self.set_input(&text);
        } else {
            self.hist_idx = None;
            let stash = std::mem::take(&mut self.hist_stash);
            self.set_input(&stash);
        }
        true
    }

    pub fn tick_spinner(&mut self) {
        self.spinner = (self.spinner + 1) % SPINNER.len();
    }

    /// Update the input textarea's border to reflect the current content mode.
    /// Call once per render frame so the style stays in sync without needing
    /// `&mut App` inside the draw closure.
    pub fn sync_input_block(&mut self) {
        let theme = self.theme;
        let hint = if theme.compact_input {
            ENTER_HINT_COMPACT
        } else {
            ENTER_HINT_FULL
        };
        let is_shell = self.input_text().trim_start().starts_with('!');
        let block = if is_shell {
            Block::default()
                .borders(Borders::TOP | Borders::BOTTOM)
                .border_type(theme.border_type)
                .border_style(Style::default().fg(Color::Red))
                .padding(Padding::horizontal(theme.inner_padding as u16))
                .title(" ! shell command — output shared with agent ")
        } else {
            Block::default()
                .borders(Borders::TOP | Borders::BOTTOM)
                .border_type(theme.border_type)
                .border_style(Style::default().fg(theme.border))
                .padding(Padding::horizontal(theme.inner_padding as u16))
                .title(hint)
                .title_style(Style::default().fg(theme.border_title))
        };
        self.input.set_block(block);
    }
}

/// Build the styled multiline input widget.
fn new_input() -> TextArea<'static> {
    let mut ta = TextArea::default();
    ta.set_block(
        Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .title(ENTER_HINT_FULL),
    );
    ta.set_cursor_line_style(Style::default());
    ta.set_placeholder_text("Type a message…");
    ta.set_placeholder_style(Style::default().fg(Color::DarkGray));
    ta
}

/// Arm/reset the InputChanged debounce when the input text changed since the
/// last observation. Called every event-loop iteration.
pub(crate) fn arm_input_debounce(app: &mut App, now: std::time::Instant) {
    let cur = app.input_text();
    if cur != app.panel_input_seen {
        app.panel_input_seen = cur;
        app.panel_input_deadline =
            Some(now + std::time::Duration::from_millis(mur_common::panel::INPUT_DEBOUNCE_MS));
    }
}

/// If the debounce deadline has passed and the text differs from the last
/// sent snapshot, consume the deadline and return the raw text to send.
pub(crate) fn take_due_input(app: &mut App, now: std::time::Instant) -> Option<String> {
    if app.panel_input_deadline.is_some_and(|d| now >= d) {
        app.panel_input_deadline = None;
        if app.panel_input_seen != app.panel_input_sent {
            app.panel_input_sent = app.panel_input_seen.clone();
            return Some(app.panel_input_sent.clone());
        }
    }
    None
}

#[cfg(test)]
impl App {
    /// Minimal fixture for unit tests. Backed by a temporary directory that is
    /// dropped on return — persist calls may fail silently (see `persist_turn`),
    /// which is fine: all state-logic tests work on the in-memory transcript.
    pub fn test_fixture() -> Self {
        let home = tempfile::tempdir().unwrap();
        let session = Session::create(home.path(), "a").unwrap();
        App::new(
            home.path().to_path_buf(),
            "a".into(),
            session,
            &super::theme::DARK,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn app() -> App {
        let home = tempdir().unwrap();
        let session = Session::create(home.path(), "a").unwrap();
        App::new(
            home.path().to_path_buf(),
            "a".into(),
            session,
            &super::super::theme::DARK,
        )
    }

    #[test]
    fn input_debounce_arms_and_fires_once() {
        use std::time::{Duration, Instant};
        let mut app = app();
        let t0 = Instant::now();

        // No edit → nothing armed, nothing due.
        arm_input_debounce(&mut app, t0);
        assert!(take_due_input(&mut app, t0 + Duration::from_secs(1)).is_none());

        // Edit arms the deadline; before expiry nothing fires.
        app.set_input("run boo");
        arm_input_debounce(&mut app, t0);
        assert!(take_due_input(&mut app, t0).is_none());

        // Continued typing re-arms (debounce reset).
        app.set_input("run book");
        arm_input_debounce(&mut app, t0 + Duration::from_millis(100));

        // After the (re-armed) deadline the latest snapshot fires exactly once.
        let due = t0 + Duration::from_millis(100 + mur_common::panel::INPUT_DEBOUNCE_MS + 1);
        assert_eq!(take_due_input(&mut app, due).as_deref(), Some("run book"));
        assert!(take_due_input(&mut app, due).is_none()); // no repeat

        // Unchanged text never re-fires even after another arm pass.
        arm_input_debounce(&mut app, due);
        assert!(take_due_input(&mut app, due + Duration::from_secs(1)).is_none());

        // Clearing the input fires an empty snapshot (panel resets to cwd mode).
        app.clear_input();
        arm_input_debounce(&mut app, due);
        let later = due + Duration::from_millis(mur_common::panel::INPUT_DEBOUNCE_MS + 1);
        assert_eq!(take_due_input(&mut app, later).as_deref(), Some(""));
    }

    /// Helper that borrows an existing TempDir so the directory survives the test.
    fn app_at(home: &tempfile::TempDir) -> App {
        let session = Session::create(home.path(), "a").unwrap();
        App::new(
            home.path().to_path_buf(),
            "a".into(),
            session,
            &super::super::theme::DARK,
        )
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
    fn parses_panel() {
        assert_eq!(parse_slash("/panel"), Some(SlashCmd::Panel(vec![])));
        assert_eq!(
            parse_slash("/panel preview out/report.html"),
            Some(SlashCmd::Panel(vec![
                "preview".to_string(),
                "out/report.html".to_string()
            ]))
        );
    }

    #[test]
    fn parse_slash_skin_variants() {
        assert_eq!(parse_slash("/skin"), Some(SlashCmd::Skin(None)));
        assert_eq!(
            parse_slash("/skin mur"),
            Some(SlashCmd::Skin(Some("mur".into())))
        );
        assert_eq!(
            parse_slash("/theme light"),
            Some(SlashCmd::Skin(Some("light".into())))
        );
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
    fn input_history_recall_cycle() {
        let home = tempdir().unwrap();
        let mut a = app_at(&home);
        a.history_record("first");
        a.history_record("second");
        a.history_record("second"); // consecutive dup skipped
        assert_eq!(a.sent_history.len(), 2);

        a.set_input("draft");
        assert!(a.history_prev());
        assert_eq!(a.input_text(), "second");
        assert!(a.history_prev());
        assert_eq!(a.input_text(), "first");
        assert!(a.history_prev()); // at oldest — swallowed, unchanged
        assert_eq!(a.input_text(), "first");
        assert!(a.history_next());
        assert_eq!(a.input_text(), "second");
        assert!(a.history_next()); // past newest — draft restored
        assert_eq!(a.input_text(), "draft");
        assert!(!a.history_next()); // not browsing — key not consumed
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

    #[test]
    fn start_new_session_clears_last_sent() {
        let home = tempdir().unwrap();
        let mut a = app_at(&home);
        a.last_sent = Some("hello".into());
        a.last_esc_at = Some(std::time::Instant::now());
        a.esc_hint = true;
        let s = Session::create(home.path(), "a").unwrap();
        a.start_new_session(s);
        assert!(a.last_sent.is_none());
        assert!(a.last_esc_at.is_none());
        assert!(!a.esc_hint);
    }
}

#[cfg(test)]
mod step_app_tests {
    use super::*;
    use crate::cmd::agent::cli::step::StepState;

    fn app() -> App {
        App::test_fixture()
    }

    #[test]
    fn step_interleaves_between_text_segments() {
        let mut a = app();
        a.begin_user_turn("hi");
        a.append_delta("reading file", false);
        a.push_step_started(
            "s1".into(),
            "read".into(),
            serde_json::json!({ "path": "a.rs" }),
        );
        // After push_step_started: prior segment frozen, step card pushed.
        // append_delta now creates a new streaming segment.
        a.append_delta("done, summary", false);

        // Expect 3 agent-role messages: frozen text, step card, new streaming text.
        let agent_msgs: Vec<_> = a
            .messages
            .iter()
            .filter(|m| m.role == Role::Agent)
            .collect();
        assert_eq!(
            agent_msgs.len(),
            3,
            "frozen segment + step card + new segment"
        );
        assert_eq!(agent_msgs[0].text, "reading file");
        assert!(!agent_msgs[0].streaming, "first segment must be frozen");
        assert!(
            agent_msgs[1].step.is_some(),
            "middle message must be a step card"
        );
        assert_eq!(agent_msgs[2].text, "done, summary");
        assert!(agent_msgs[2].streaming, "new segment must be streaming");
    }

    #[test]
    fn step_before_text_drops_empty_placeholder() {
        let mut a = app();
        a.begin_user_turn("hi");
        // No delta yet — placeholder is empty.
        a.push_step_started("s1".into(), "bash".into(), serde_json::json!({}));
        // Empty placeholder dropped; only step card remains as agent message.
        let agent_msgs: Vec<_> = a
            .messages
            .iter()
            .filter(|m| m.role == Role::Agent)
            .collect();
        assert_eq!(
            agent_msgs.len(),
            1,
            "empty placeholder dropped, only step card"
        );
        assert!(agent_msgs[0].step.is_some(), "must be step card");
    }

    #[test]
    fn update_step_completed_marks_card_done() {
        let mut a = app();
        a.begin_user_turn("hi");
        a.push_step_started(
            "s1".into(),
            "bash".into(),
            serde_json::json!({ "cmd": "ls" }),
        );
        a.update_step_completed("s1", true, "foo.rs\n".into(), false, 7, None, 42);
        let card = a
            .messages
            .iter()
            .find_map(|m| m.step.as_ref())
            .expect("step card");
        assert_eq!(card.state, StepState::Done);
        assert_eq!(card.duration_ms, Some(42));
        assert_eq!(card.output, "foo.rs\n");
    }

    #[test]
    fn tool_turn_reply_is_pushed_not_dropped() {
        let mut a = app();
        a.begin_user_turn("read the file");
        a.push_step_started(
            "s1".into(),
            "read".into(),
            serde_json::json!({"path":"a.rs"}),
        );
        a.update_step_completed("s1", true, "ok".into(), false, 2, None, 5);
        // No streaming segment now (tool turn, no text deltas).
        a.finish_agent_turn("here is the summary".into(), Some("t1".into()));
        let last = a.messages.last().unwrap();
        assert!(last.step.is_none());
        assert_eq!(last.role, Role::Agent);
        assert_eq!(last.text, "here is the summary");
        assert!(!last.streaming);
        assert!(last.rendered.is_some());
    }

    #[test]
    fn multi_segment_finish_sets_trailing_keeps_frozen() {
        let mut a = app();
        a.begin_user_turn("hi");
        a.append_delta("looking at it", false);
        a.push_step_started("s1".into(), "read".into(), serde_json::json!({}));
        a.append_delta("here is the answer", false);
        // reply = final iteration text only
        a.finish_agent_turn("here is the answer".into(), Some("t1".into()));
        let segs: Vec<_> = a
            .messages
            .iter()
            .filter(|m| m.role == Role::Agent && m.step.is_none())
            .collect();
        assert_eq!(segs[0].text, "looking at it"); // frozen, untouched
        assert_eq!(segs[1].text, "here is the answer"); // trailing got reply
        assert!(!segs[1].streaming);
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
        assert_eq!(
            esc_action(recent(), true, true),
            EscAction::CancelAndRestore
        );
    }

    #[test]
    fn esc_cancel_restore_on_second_press_streaming_has_text() {
        assert_eq!(
            esc_action(recent(), true, false),
            EscAction::CancelAndRestore
        );
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

#[cfg(test)]
mod overlay_key_action_tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    #[test]
    fn esc_closes() {
        assert_eq!(
            overlay_key_action(KeyCode::Esc, KeyModifiers::NONE),
            OverlayKeyAction::Close
        );
    }

    #[test]
    fn enter_closes() {
        assert_eq!(
            overlay_key_action(KeyCode::Enter, KeyModifiers::NONE),
            OverlayKeyAction::Close
        );
    }

    #[test]
    fn ctrl_d_closes_and_quits() {
        assert_eq!(
            overlay_key_action(KeyCode::Char('d'), KeyModifiers::CONTROL),
            OverlayKeyAction::CloseAndQuit
        );
    }

    #[test]
    fn plain_d_is_ignored_not_quit() {
        assert_eq!(
            overlay_key_action(KeyCode::Char('d'), KeyModifiers::NONE),
            OverlayKeyAction::Ignore
        );
    }

    #[test]
    fn other_chars_are_ignored_never_inserted() {
        assert_eq!(
            overlay_key_action(KeyCode::Char('x'), KeyModifiers::NONE),
            OverlayKeyAction::Ignore
        );
    }

    #[test]
    fn arrow_keys_are_ignored() {
        assert_eq!(
            overlay_key_action(KeyCode::Down, KeyModifiers::NONE),
            OverlayKeyAction::Ignore
        );
    }

    #[test]
    fn ctrl_c_is_ignored_overlay_only_recognises_ctrl_d() {
        assert_eq!(
            overlay_key_action(KeyCode::Char('c'), KeyModifiers::CONTROL),
            OverlayKeyAction::Ignore
        );
    }
}

#[cfg(test)]
mod awaiting_tests {
    use super::*;

    #[test]
    fn mark_and_clear_awaiting_by_step_id() {
        let mut a = App::test_fixture();
        a.begin_user_turn("edit it");
        a.push_step_started(
            "s1".into(),
            "edit".into(),
            serde_json::json!({"file_path":"a.rs"}),
        );
        a.update_step_completed("s1", true, "ok".into(), false, 2, None, 5);
        a.mark_card_awaiting("s1");
        let card = a.messages.iter().find_map(|m| m.step.as_ref()).unwrap();
        assert!(card.awaiting_hitl);
        a.clear_card_awaiting("s1");
        let card = a.messages.iter().find_map(|m| m.step.as_ref()).unwrap();
        assert!(!card.awaiting_hitl);
    }
}

#[cfg(test)]
mod reasoning_kept_tests {
    use super::*;

    #[test]
    fn thinking_survives_turn_finish() {
        let mut a = App::test_fixture();
        a.begin_user_turn("hi");
        a.append_delta("let me think", true); // thinking delta
        a.append_delta("the answer", false);
        a.finish_agent_turn("the answer".into(), Some("t1".into()));
        let last = a.messages.last().unwrap();
        assert_eq!(last.role, Role::Agent);
        assert_eq!(last.thinking, "let me think"); // not cleared
        assert!(!last.streaming);
    }
}

#[cfg(test)]
mod footer_state_tests {
    use super::*;

    #[test]
    fn apply_usage_accumulates_session_and_sets_turn() {
        let mut a = App::test_fixture();
        a.apply_usage(
            &serde_json::json!({ "input_tokens": 100, "output_tokens": 20, "context_tokens": 100 }),
        );
        a.apply_usage(
            &serde_json::json!({ "input_tokens": 50, "output_tokens": 10, "context_tokens": 150 }),
        );
        assert_eq!(a.turn_in, 50);
        assert_eq!(a.turn_out, 10);
        assert_eq!(a.session_in, 150);
        assert_eq!(a.session_out, 30);
        assert_eq!(a.ctx_tokens, 150);
    }

    #[test]
    fn begin_user_turn_resets_turn_counters_and_arms_clock() {
        let mut a = App::test_fixture();
        // Prime some prior-turn state.
        a.apply_usage(&serde_json::json!({ "input_tokens": 100, "output_tokens": 20 }));
        a.begin_user_turn("hi");
        assert_eq!(a.turn_in, 0, "turn_in reset");
        assert_eq!(a.turn_out, 0, "turn_out reset");
        assert!(!a.saw_step_this_turn, "saw_step reset");
        assert!(a.turn_started.is_some(), "clock armed");
        // session accumulators must NOT be cleared by begin_user_turn.
        assert_eq!(a.session_in, 100, "session_in survives begin_user_turn");
        assert_eq!(a.session_out, 20, "session_out survives begin_user_turn");
    }

    #[test]
    fn begin_user_turn_clears_stale_pending_suggestions() {
        let mut a = App::test_fixture();
        // Simulate a prior turn that set suggestions but never revealed them
        // (e.g., the turn ended in Err or was cancelled via Ctrl+C).
        a.pending_suggestions = vec![super::super::suggest::Suggestion {
            text: "stale suggestion".to_string(),
            desc: None,
        }];
        a.begin_user_turn("new turn");
        assert!(
            a.pending_suggestions.is_empty(),
            "pending_suggestions must be cleared at the start of each turn"
        );
    }

    #[test]
    fn finish_agent_turn_clears_clock() {
        let mut a = App::test_fixture();
        a.begin_user_turn("hi");
        assert!(a.turn_started.is_some());
        a.finish_agent_turn("ok".into(), None);
        assert!(a.turn_started.is_none(), "clock cleared after finish");
    }

    #[test]
    fn finish_partial_clears_clock() {
        let mut a = App::test_fixture();
        a.begin_user_turn("hi");
        assert!(a.turn_started.is_some());
        a.finish_partial();
        assert!(a.turn_started.is_none());
    }

    #[test]
    fn context_tokens_update_on_apply() {
        let mut a = App::test_fixture();
        a.apply_usage(&serde_json::json!({ "input_tokens": 10, "output_tokens": 5 }));
        assert_eq!(a.ctx_tokens, 0, "no context_tokens field → unchanged");
        a.apply_usage(
            &serde_json::json!({ "input_tokens": 10, "output_tokens": 5, "context_tokens": 42000 }),
        );
        assert_eq!(a.ctx_tokens, 42000);
    }

    #[test]
    fn old_runtime_hitl_without_steps_shows_hint_once() {
        let mut a = App::test_fixture();
        a.begin_user_turn("do it");
        a.saw_hitl_this_turn = true; // hitl arrived, no step events => old runtime
        a.maybe_step_hint();
        assert!(a.step_hint_shown);
        let n = a
            .messages
            .iter()
            .filter(|m| m.role == Role::System && m.text.contains("restart"))
            .count();
        assert_eq!(n, 1);
        // second such turn: not shown again
        a.begin_user_turn("again");
        a.saw_hitl_this_turn = true;
        a.maybe_step_hint();
        let n2 = a
            .messages
            .iter()
            .filter(|m| m.role == Role::System && m.text.contains("restart"))
            .count();
        assert_eq!(n2, 1);
    }

    #[test]
    fn new_runtime_with_steps_shows_no_hint() {
        let mut a = App::test_fixture();
        a.begin_user_turn("do it");
        a.saw_hitl_this_turn = true;
        a.saw_step_this_turn = true; // new runtime emitted step events
        a.maybe_step_hint();
        assert!(!a.step_hint_shown);
        assert!(!a.messages.iter().any(|m| m.text.contains("restart")));
    }
}

#[cfg(test)]
mod over_budget_tests {
    use super::*;

    #[test]
    fn over_budget_only_when_priced_and_at_or_past_cap() {
        let mut a = App::test_fixture();
        a.pricing = super::super::footer::Pricing {
            in_per_1k: Some(3.0),
            out_per_1k: Some(15.0),
            window: None,
        };
        a.session_in = 1000;
        a.session_out = 1000; // $18.00 spent
        a.budget_usd = None;
        assert!(!a.over_budget()); // no cap
        a.budget_usd = Some(20.0);
        assert!(!a.over_budget()); // under
        a.budget_usd = Some(18.0);
        assert!(a.over_budget()); // at cap
        a.budget_usd = Some(5.0);
        assert!(a.over_budget()); // over
        a.pricing = super::super::footer::Pricing::default(); // unpriced → fail OPEN
        a.budget_usd = Some(0.01);
        assert!(!a.over_budget());
    }
}

#[cfg(test)]
mod session_cost_tests {
    use super::*;

    #[test]
    fn session_cost_uses_pricing_over_session_tokens_or_none() {
        let mut a = App::test_fixture();
        a.pricing = super::super::footer::Pricing {
            in_per_1k: Some(3.0),
            out_per_1k: Some(15.0),
            window: None,
        };
        a.session_in = 1000;
        a.session_out = 1000;
        // (1000/1000*3) + (1000/1000*15) = 18.0
        assert_eq!(a.session_cost(), Some(18.0));
        // unpriced model → None (fail-open: unknown cost never blocks)
        a.pricing = super::super::footer::Pricing::default();
        assert_eq!(a.session_cost(), None);
    }
}
