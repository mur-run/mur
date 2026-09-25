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
use super::step::StepState;
use super::stream::HitlRequest;
use super::theme::Theme;
use super::welcome::{MascotMode, resolve_mascot_mode};

/// Spinner frames shown while the agent is generating.
pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Compact composer-border hint. Shift+Enter doesn't need an OS-specific
/// label (the key is "Shift" everywhere); the Alt/Option fallback chord only
/// shows up in the full hint below, where there's room to spell it out.
const ENTER_HINT_COMPACT: &str = " message — Enter · Shift+Enter · /help ";

/// Full composer-border hint. macOS calls the modifier "Option" even though
/// it's still crossterm's `ALT` — every other OS calls it "Alt".
#[cfg(target_os = "macos")]
pub(super) const ENTER_HINT_FULL: &str = " message — Enter to send · Shift+Enter newline (Option+Enter also works) · Ctrl+V image · Ctrl+O transcript · /help · Ctrl+D quit";
#[cfg(not(target_os = "macos"))]
pub(super) const ENTER_HINT_FULL: &str = " message — Enter to send · Shift+Enter newline (Alt+Enter also works) · Ctrl+V image · Ctrl+O transcript · /help · Ctrl+D quit";

/// Assumed terminal width until the event loop reports the real one.
const DEFAULT_WIDTH: u16 = 80;

/// Columns a bordered block spends on its own corners, unavailable to a title.
const BORDER_CORNERS: usize = 2;

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

/// An interactive child the main loop must run with the terminal handed over.
/// Set by a slash command; taken and cleared by the loop.
///
/// `Debug` is derived and leaks nothing: the only non-trivial field is a
/// `LoginLock`, whose `std::fs::File` prints a descriptor and path, never
/// contents. A `tracing::debug!(?req)` should not be a compile error.
#[derive(Debug)]
pub struct HandoverRequest {
    /// Non-interactive commands to run, in order, in the same suspension and
    /// immediately before `argv`. Their exit status is deliberately ignored:
    /// the one caller is `/login`'s `claude auth logout`, which exits non-zero
    /// on a CLI that was already signed out — a state the login that follows
    /// handles perfectly well. Empty for most requests.
    pub pre: Vec<Vec<String>>,
    pub argv: Vec<String>,
    /// What to name in the before/after system messages.
    pub label: String,
    /// Held for the child's lifetime; dropped with the request.
    pub _lock: Option<crate::cmd::agent::cli::login::LoginLock>,
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
    /// Gates that arrived while `hitl` was occupied. A P3 runtime asks once
    /// per response with N calls; the runtime awaits their answers in order
    /// against one deadline, so showing them one at a time is exact. Drained
    /// by `mod::promote_queued_hitl` whenever the slot empties.
    pub hitl_queue: std::collections::VecDeque<(String, HitlRequest)>,
    /// When the last HITL decision was resolved, for swallowing a stale
    /// decision key: if a gate auto-resolves (read lane / /auto) just as the
    /// operator presses `y`, that keystroke would otherwise land in the
    /// composer as text.
    pub hitl_resolved_at: Option<std::time::Instant>,
    /// Which row of the approval menu is highlighted. A single stray keystroke
    /// must never hand a tool blanket approval, so the session-wide grants are
    /// not bare keys at all: they are menu rows the operator must move to and
    /// confirm with Enter. Reset to 0 (`Yes`, this call only) on every new
    /// gate, so the row under a reflex Enter is always the narrowest one.
    pub hitl_selected: usize,
    /// Scroll offset into the approval modal's body, in wrapped rows. Reset on
    /// every new gate so a fresh request always opens at the top; the renderer
    /// clamps it to the content and hands back what it used.
    pub hitl_scroll: u16,
    /// Body rows the approval modal last had room to show. The renderer reports
    /// it so PgUp/PgDn can page by the real window instead of a fixed guess
    /// that could step over unread rows; 0 until the modal has drawn once.
    pub hitl_page: u16,
    pub session: Session,
    /// Cached live-channel id + state for status bar. Refreshed after each
    /// persisted turn on resume/switch. `None` until first append.
    pub channel: Option<ChannelMeta>,
    /// Lines scrolled up from the bottom (0 = pinned to newest).
    pub scroll_back: u16,
    /// Transcript viewport height (rows), captured each render so PageUp/Down
    /// move a screenful and `scroll_back` can be clamped to the real maximum.
    pub scroll_page: u16,
    /// Transcript wrap width (columns), captured each render. A paste carries
    /// no hint of whether its newlines were typed or painted by the pane's
    /// wrap, so `paste::unwrap_soft_breaks` needs the width the transcript was
    /// wrapped AT to tell one from the other. 0 until the first render, which
    /// disables unwrapping — an unknown width must never edit a paste.
    pub wrap_width: u16,
    /// User adjustment to the chooser band height (Ctrl+↑/↓ while the
    /// chooser is open), in rows relative to the auto-computed height.
    /// Persists for the session so a preferred size sticks between turns.
    pub chooser_grow: i16,
    pub spinner: usize,
    pub should_quit: bool,
    /// Session-wide auto-approval of every tool call. ON by default: a
    /// session starts approving, `--ask` or `/auto off` makes it ask first.
    /// Never persisted — the next `mur agent cli` starts from the default
    /// again, not from where this one left off.
    pub auto_approve: bool,
    /// Tools the user marked "always allow" for THIS session via the HITL
    /// modal's `[a]` key. Same lifetime rules as `auto_approve`.
    pub session_tool_allow: HashSet<String>,
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
    /// the newest screenful, since a message is flushed only once the live
    /// band overflows (see `ui::flush_finished`).
    pub flushed_upto: usize,
    /// Bytes of `messages[flushed_upto].text` already flushed into scrollback
    /// as complete markdown blocks while that turn was still streaming (0 when
    /// nothing of it is committed). The live band paints only what follows.
    pub flushed_bytes: usize,
    /// Hash of the exact committed prefix, so a message whose text was
    /// replaced (`finish_agent_turn` installs the authoritative reply) or
    /// dropped (`fail_turn`) can be detected instead of splicing a remainder
    /// onto text that never had that prefix.
    pub flushed_hash: u64,
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
    /// The running `!cmd`, if any, and its generation. `is_running()` is what
    /// makes Ctrl-C end the shell rather than the agent turn (D3), and what
    /// keeps the spinner ticking for a shell-only command (§3.6).
    pub shell: super::shell::ShellState,
    pub last_sent: Option<String>,
    /// #8 / proposal 3a: the user request whose tool approval auto-denied at
    /// timeout. A pre-execution timeout deny has NO side effects (the tool
    /// never ran), so replaying is safe. Holds the `last_sent` snapshot so the
    /// modal's `[r]` can one-shot re-run the request that hit the wall, instead
    /// of the old "arrived too late — re-run the request" dead end. `None`
    /// unless an expired-at-timeout deny is pending a retry; cleared once
    /// consumed by `[r]` or superseded by the next sent turn.
    pub expired_retry: Option<String>,
    /// `(mime, base64)` of an image staged for the next message — either a
    /// clipboard screenshot (Ctrl+V) or an image file the terminal pasted as a
    /// path (Cmd+V / drag-drop). Sent as an inline image part, cleared on send.
    pub pending_image: Option<(String, String)>,
    /// Mascot color/animation mode, resolved once at startup from the theme
    /// and terminal capabilities (NO_COLOR / non-TTY / TERM=dumb → static).
    pub mascot_mode: MascotMode,
    /// True while the terminal is focused. Driven by crossterm focus events;
    /// used to suppress notifications while the user is watching.
    pub focused: bool,
    /// Wall-clock instant when the current agent turn began (set in
    /// `begin_user_turn`, cleared in `finish_agent_turn` / `fail_turn`).
    pub turn_started: Option<std::time::Instant>,
    /// Fingerprint of the open-item set the user was last told about, so the
    /// end-of-turn notice fires on change rather than on every turn.
    pub open_items_fp: Option<u64>,
    /// Cumulative token counts for this session (all turns combined).
    pub session_in: u64,
    pub session_out: u64,
    /// Token counts for the most recent completed turn.
    pub turn_in: u64,
    pub turn_out: u64,
    /// Last-known context fill from the runtime's `Task.usage.context_tokens`.
    /// Effort set with `/effort` this session, if any. Kept apart from the
    /// profile value so `effective_effort` can report WHICH one is in force —
    /// the user needs to know whether their change outlives the session.
    pub session_effort: Option<mur_common::llm::Effort>,
    pub ctx_tokens: u64,
    /// Pricing for the model that answered the LAST turn — not necessarily the
    /// one this agent is configured with, because the runtime's fallback chain
    /// substitutes another when a candidate is unreachable, out of credit, or
    /// serving a retired model id. Re-resolved per turn from `usage.model_ref`.
    pub pricing: super::footer::Pricing,
    /// Every registry entry's pricing, so a substitution can be re-priced
    /// without touching disk mid-turn. `None` in tests and in plain mode.
    pub pricing_book: Option<super::PricingBook>,
    /// Registry key of the model that answered most recently, once it has
    /// differed from the configured one. Drives the substitution notice, and
    /// remembers what we already told the user so a long session does not
    /// repeat itself every turn.
    pub answered_key: Option<String>,
    /// Set to `true` when `StepStarted` fires this turn; used by the footer
    /// to distinguish "pure chat" from "agentic" turns.
    pub saw_step_this_turn: bool,
    /// A tool was *approved* this turn (any runtime). Paired with
    /// `saw_step_this_turn` to detect an old runtime that ran a tool but
    /// streamed no step events.
    ///
    /// Set at the decision, not at the request: a denied call never executes,
    /// so a missing step stream says nothing about the runtime's age. Setting
    /// it on arrival made every deny print "this agent ran a tool without
    /// streaming step detail" about a tool that did not run (#940).
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
    /// Terminal width in columns, refreshed once per event-loop pass. Anything
    /// that has to fit on one row — the composer hint, the status line, a tool
    /// card's arg hint — sizes itself against this instead of a fixed column
    /// count that clipped mid-word on a wide terminal and overflowed a narrow
    /// one. 80 until the first refresh.
    pub width: u16,
    /// Live completion menu (slash commands / agent skills). `None` = closed.
    /// Derived from the input text — recomputed on every edit by `mod.rs`.
    pub completion: Option<CompletionState>,
    /// This agent's skills as menu candidates, loaded once at startup.
    pub skills: Vec<Candidate>,
    /// Argument lists for the completion menu — effort levels, registry
    /// models, secret KEYs, note names. Rebuilt after every slash command;
    /// `compute` is pure, so this is where that I/O lives.
    pub menu_ctx: super::complete::MenuContext,
    /// Executable names on `$PATH` for `!` completion. `None` until the first
    /// `!` completion asks; scanned once per session after that.
    pub path_bins: Option<Vec<String>>,
    /// Replies captured from a `suggest_replies` tool call this turn, revealed
    /// after the turn finishes (see `reveal_suggestions`).
    pub pending_suggestions: Vec<super::suggest::Suggestion>,
    /// The single suggestion currently shown as ghost placeholder text, if any.
    pub suggestion_ghost: Option<String>,
    /// Set when the visible transcript no longer matches the conversation
    /// (/clear, /channels switch): the event loop wipes screen + scrollback
    /// and re-anchors a fresh viewport before the next draw.
    pub wants_screen_wipe: bool,
    /// A channel being live-tailed (`/channels N --follow`) — someone else's
    /// conversation, not this pane's. `None` = not following.
    pub follow: Option<super::follow::Follow>,
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
    /// Fleet rail, when `--fleet` is on. `None` for an ordinary murmur.
    pub fleet: Option<super::fleet_rail::FleetRail>,
    /// `step_id` of an in-flight `fleet_run` tool step that auto-armed the
    /// rail/follow, so its `StepCompleted` can close them out. `None` when no
    /// delegated fleet run is executing.
    pub auto_fleet_step: Option<String>,
    /// Set by a slash command (`/login`'s escalating repair) that needs the
    /// real terminal for an interactive child, e.g. `claude auth login`. The
    /// main loop takes and clears this — `handle_slash` has no access to
    /// `terminal`/`events` to run the handover itself.
    pub pending_handover: Option<HandoverRequest>,
    /// A `/secret KEY` waiting for the main loop to read its value with the
    /// terminal handed over. Never holds the value itself.
    pub pending_secret_prompt: Option<String>,
    /// A `/secret KEY --delete` waiting to be carried out.
    pub pending_secret_delete: Option<String>,
    /// A destructive memory operation waiting for a typed yes/no.
    ///
    /// Delete and demote of a **permanent instruction**, and an edit that
    /// deliberately overflows the budget, all require explicit confirmation
    /// (plan §7). Held as data rather than driven from the key handler so the
    /// decision logic stays in `memory_cmds` and testable: `submit` consumes
    /// the next typed line as the answer, and anything that is not an explicit
    /// yes cancels — a reflex Enter must never destroy an instruction the user
    /// deliberately asked to keep.
    pub pending_memory_confirm: Option<super::memory_cmds::PendingMemoryOp>,
    /// Registered monitor count, including healthy `sleeping` monitors, as of
    /// the last refresh — never computed during render.
    pub monitor_total: usize,
    /// Count of registered monitors with a live condition, as of the last
    /// refresh — never computed during render.
    pub monitor_conditions: usize,
    /// When monitor counts were last refreshed; `None` before the first
    /// refresh, so it always runs once per session.
    pub last_monitor_refresh: Option<std::time::Instant>,
    /// Result set of the most recent `/search`, so `/search --expand <id>` can
    /// show a full chunk without re-running the query. Replaced by each new
    /// search; never consulted for anything but expansion.
    pub search_snapshot: Option<super::search::SearchSnapshot>,
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
            hitl_queue: std::collections::VecDeque::new(),
            hitl_resolved_at: None,
            hitl_selected: 0,
            hitl_scroll: 0,
            hitl_page: 0,
            session,
            channel: None,
            scroll_back: 0,
            scroll_page: 0,
            wrap_width: 0,
            chooser_grow: 0,
            spinner: 0,
            should_quit: false,
            auto_approve: true,
            session_tool_allow: HashSet::new(),
            persist_warned: false,
            panel: None,
            panel_stream: false,
            cwd: std::env::current_dir().ok(),
            theme,
            last_esc_at: None,
            esc_hint: false,
            needs_full_redraw: false,
            render_mode: RenderMode::Inline,
            flushed_upto: 0,
            flushed_bytes: 0,
            flushed_hash: 0,
            overlay_open: false,
            overlay_text: None,
            last_ctrl_c_at: None,
            ctrl_c_hint: false,
            shell: Default::default(),
            last_sent: None,
            expired_retry: None,
            pending_image: None,
            // Resolve color/animation once: env + TTY don't change mid-session.
            mascot_mode: resolve_mascot_mode(theme, std::io::stdout().is_terminal()),
            // Assume focused at startup; crossterm corrects it on the first
            // FocusLost. (Terminals that don't report focus stay `true` → no
            // notifications, which is the safe default.)
            focused: true,
            turn_started: None,
            open_items_fp: None,
            session_in: 0,
            session_out: 0,
            turn_in: 0,
            turn_out: 0,
            session_effort: None,
            ctx_tokens: 0,
            pricing: super::footer::Pricing::default(),
            pricing_book: None,
            answered_key: None,
            saw_step_this_turn: false,
            saw_hitl_this_turn: false,
            step_hint_shown: false,
            budget_usd: None,
            auto_reads: false,
            cards_expanded: false,
            width: DEFAULT_WIDTH,
            completion: None,
            skills: Vec::new(),
            menu_ctx: super::complete::MenuContext::default(),
            path_bins: None,
            pending_suggestions: Vec::new(),
            suggestion_ghost: None,
            wants_screen_wipe: false,
            follow: None,
            sent_history: Vec::new(),
            hist_idx: None,
            hist_stash: String::new(),
            panel_input_seen: String::new(),
            panel_input_sent: String::new(),
            panel_input_deadline: None,
            fleet: None,
            auto_fleet_step: None,
            pending_handover: None,
            pending_secret_prompt: None,
            pending_secret_delete: None,
            pending_memory_confirm: None,
            monitor_total: 0,
            monitor_conditions: 0,
            last_monitor_refresh: None,
            search_snapshot: None,
        }
    }

    /// The rail's current view, or `None` when `--fleet` is off.
    pub fn fleet_view(&self) -> Option<&super::fleet_rail::RailView> {
        self.fleet.as_ref().map(|f| f.view())
    }

    pub fn load_history(&mut self, turns: Vec<TurnRecord>) {
        let width = self.body_cols();
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
                self.messages.push(ChatMsg::agent_rendered(t.text, width));
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
        // The long hint only goes up when it fits. Ratatui clips a Block title
        // that overruns the block, and the clip lands mid-word — the composer
        // border used to end in "… /help · Ct" on anything but a very wide
        // terminal. Two border corners plus the pane padding are unavailable
        // to the title.
        let budget = usize::from(self.width)
            .saturating_sub(usize::from(theme.inner_padding) * 2 + BORDER_CORNERS);
        let hint = if theme.compact_input || ENTER_HINT_FULL.chars().count() > budget {
            ENTER_HINT_COMPACT
        } else {
            ENTER_HINT_FULL
        };
        let is_shell = self.input_text().trim_start().starts_with('!');
        let block = if is_shell {
            Block::default()
                .borders(Borders::TOP)
                .border_type(theme.border_type)
                .border_style(theme.error)
                .padding(composer_padding(theme))
                .title(" ! shell command — output shared with agent ")
        } else {
            Block::default()
                .borders(Borders::TOP)
                .border_type(theme.border_type)
                .border_style(theme.border)
                .padding(composer_padding(theme))
                .title(hint)
                .title_style(theme.muted)
        };
        self.input.set_block(block);
    }
}

/// Build the styled multiline input widget.
/// The composer's inner padding: the skin's horizontal padding, plus one
/// blank row under the text so the input does not sit jammed against the
/// status bar. Nothing above it — the titled rule is its own separation, and
/// a blank there too read as a hole (field report).
fn composer_padding(theme: &Theme) -> Padding {
    let h = u16::from(theme.inner_padding);
    Padding::new(h, h, 0, COMPOSER_PAD_BELOW)
}

/// Blank rows between the composer text and the status bar.
/// `ui::INPUT_H_MIN` counts them; change both together.
pub(super) const COMPOSER_PAD_BELOW: u16 = 1;

fn new_input() -> TextArea<'static> {
    let mut ta = TextArea::default();
    ta.set_block(
        Block::default()
            .borders(Borders::TOP)
            .padding(Padding::new(0, 0, 0, COMPOSER_PAD_BELOW))
            .title(ENTER_HINT_COMPACT),
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
            &super::theme::ANSI,
        )
    }
}

mod keys;
mod msg;
mod session;
mod slash;
mod transcript;
mod usage;
pub(super) use keys::{
    ESC_DOUBLE_WINDOW, EscAction, OverlayKeyAction, esc_action, overlay_key_action,
};
pub(super) use msg::{ChatMsg, Role, Severity};
pub(super) use slash::{ChannelRef, SlashCmd, parse_slash};

#[cfg(test)]
mod tests;
