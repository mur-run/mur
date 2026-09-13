//! Transcript and step-card methods on `App`, moved out of `app/mod.rs` for CLAUDE.md §4's 800-line rule.
//! Pure movement: every item below is verbatim.

use super::*;

impl App {
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
        self.persist_turn("user", text, None, &[]);
        self.begin_turn()
    }

    /// Start a turn the transcript already shows — a `!cmd` whose Shell card
    /// is its entry. Everything `begin_user_turn` does except the User bubble
    /// and the `"user"` channel event.
    pub fn begin_turn(&mut self) -> String {
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
        let width = self.body_cols();
        let mut body = None;
        if let Some(m) = self.streaming_agent_mut() {
            if !reply.is_empty() {
                m.text = reply;
            }
            let (text, settlement) = super::super::settlement::split(&m.text);
            m.text = text;
            m.settlement = settlement;
            m.streaming = false;
            m.rendered = Some(markdown::render(&m.text, width).lines);
            body = Some(m.text.clone());
        } else if self.streaming && !reply.is_empty() {
            // Tool-using turns run the agentic loop, which doesn't stream text
            // deltas — the empty placeholder was dropped when the first step card
            // arrived, so there's no trailing segment. Push the final reply as its
            // own finished message instead of dropping it.
            // Guard: self.streaming is false after finish_partial() so stale
            // Done events from cancelled tasks are still silently ignored.
            self.messages
                .push(ChatMsg::agent_rendered(reply.clone(), width));
            self.scroll_back = 0;
            body = Some(reply);
        }
        if let Some(b) = body {
            if let Some(tid) = &task_id {
                self.context_task_id = Some(tid.clone());
            }
            // Persist the quick-reply options offered this turn alongside the
            // reply so channel history is not lossy about what was offered
            // (#716). Read (not taken) here: `reveal_suggestions` still runs
            // after this to surface them in the composer.
            let offered = self.pending_suggestions.clone();
            self.persist_turn("agent", &b, task_id.as_deref(), &offered);
        }
        self.streaming = false;
        self.current_task_id = None;
        self.turn_started = None;
        self.resolve_open_steps("turn ended without a result for this call");
        self.note_open_items_if_changed();
    }

    /// Origins the user has muted. Read fresh, so `mur open mute` in another
    /// terminal takes effect on the next turn rather than the next restart.
    pub fn muted_origins(&self) -> Vec<String> {
        mur_common::config::Config::load_or_default(&self.home.join("config.yaml"))
            .open_items
            .muted
    }

    /// After a turn, say what is outstanding — but only if the set changed.
    ///
    /// Repeating the same three items after every turn is how a status line
    /// becomes wallpaper. Staying silent when nothing moved is what keeps the
    /// line worth reading on the turn something does. Muted sources are removed
    /// before the comparison, so muting silences this too — the turn notice is
    /// where noise costs most, because it interrupts.
    pub fn note_open_items_if_changed(&mut self) {
        let (visible, _) = crate::open_items::partition(
            crate::open_items::collect(&self.home),
            &self.muted_origins(),
        );
        let (visible, stale) = crate::open_items::split_stale(visible, chrono::Utc::now());
        let fp = crate::open_items::fingerprint(&visible);
        if self.open_items_fp == Some(fp) {
            return;
        }
        self.open_items_fp = Some(fp);
        if let Some(line) = crate::open_items::summary_line(&visible, stale.len()) {
            self.push_system(line);
        }
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
                let rendered =
                    Some(markdown::render(&self.messages[i].text, self.body_cols()).lines);
                self.messages[i].streaming = false;
                self.messages[i].rendered = rendered;
            }
        }
        self.messages
            .push(ChatMsg::tool(super::super::step::StepCard::new(
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
        denied: bool,
        running: bool,
    ) {
        if let Some(card) = self
            .messages
            .iter_mut()
            .rev()
            .find_map(|m| m.step.as_mut().filter(|c| c.id == step_id))
        {
            let outcome = match (ok, denied, running) {
                (_, true, _) => super::super::step::CallOutcome::Denied,
                (_, _, true) => super::super::step::CallOutcome::Running,
                (true, _, _) => super::super::step::CallOutcome::Ok,
                (false, _, _) => super::super::step::CallOutcome::Failed,
            };
            card.complete(outcome, output, truncated, full_len, error, duration_ms);
        }
    }

    /// Close every step card still spinning, because the turn that owned them
    /// has ended.
    ///
    /// A card leaves `Running` only when its matching `tool/result` arrives.
    /// A turn that dies first — the runtime went idle, the dial timed out, the
    /// task failed — never sends one, so the card spins forever and the
    /// transcript keeps claiming a tool is running long after nothing is.
    /// That is the same failure as an approval gate with no visible surface:
    /// the screen and the truth disagree, and the screen is the one the
    /// operator believes.
    ///
    /// Only cards still in the live band repaint; anything already committed
    /// to native scrollback is frozen. The spinning card is by definition the
    /// most recent one, so in practice it is the one that gets fixed.
    pub fn resolve_open_steps(&mut self, reason: &str) {
        for card in self
            .messages
            .iter_mut()
            .skip(self.flushed_upto)
            .filter_map(|m| m.step.as_mut())
            .filter(|c| c.state == StepState::Running)
        {
            card.abandon(reason.to_string());
        }
    }

    /// Flag the card with this `step_id` as awaiting a HITL decision, so it
    /// renders the inline approval row. Whether that row can actually be SEEN
    /// is a separate question — ask [`App::hitl_inline_visible`] at render
    /// time, not once at attach time.
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

    /// Is the inline approval row for the open gate actually on screen?
    ///
    /// Only a card still in the live band can repaint: messages below
    /// `flushed_upto` are committed to the terminal's native scrollback and
    /// their rows are frozen forever. The renderer suppresses the centered
    /// modal when this is true, so a wrong `true` costs the operator every
    /// surface at once — no modal, no inline row, and a status line that
    /// cannot name the tool. Two ways that used to happen: the gate fires
    /// before the card exists (common), and the card is flushed to scrollback
    /// either before the gate opens or while it is still open.
    ///
    /// Recompute it per frame; do not cache.
    pub fn hitl_inline_visible(&self, step_id: Option<&str>) -> bool {
        let Some(sid) = step_id else { return false };
        self.messages.iter().skip(self.flushed_upto).any(|m| {
            m.step
                .as_ref()
                .is_some_and(|c| c.id == sid && c.awaiting_hitl)
        })
    }

    /// Mark the card with this `step_id` as auto-approved — by the read lane
    /// (`--auto-reads`) or by a session allow (`[a]`). Call BEFORE moving `req`
    /// into `app.hitl`.
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
        self.resolve_open_steps(err);
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
        self.resolve_open_steps("the turn this call belonged to no longer exists");
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
            &[],
        );
    }
}
