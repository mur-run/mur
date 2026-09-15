//! Cost, budget and usage accounting on `App`, moved out of `app/mod.rs` for CLAUDE.md §4's 800-line rule.
//! Pure movement: every item below is verbatim.

use super::*;

impl App {
    /// Estimated cumulative session cost in USD, or `None` if the model's
    /// pricing is unknown. Used by Task 2 to gate new turns against
    /// `budget_usd`. Fail-open: `None` means "can't price it, allow the turn".
    pub fn session_cost(&self) -> Option<f64> {
        super::super::footer::turn_cost(
            &self.pricing,
            &super::super::footer::UsageCounts {
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

    /// Refresh `monitor_conditions` from the monitor store, at most once per
    /// `MONITOR_REFRESH_SECS` — opening SQLite on every frame would be a
    /// per-keystroke file open. Uses `open_existing`, not `open`: a home
    /// that has never run `mur monitor` has no `monitors.db`, and this timer
    /// runs for the life of every murmur session whether or not the feature
    /// has ever been touched — the footer must not be the thing that
    /// materialises the database for a user who never asked for one. A
    /// missing store, or one that fails to open, leaves the previous count:
    /// the footer is not a diagnostic surface.
    pub fn refresh_monitor_counts(&mut self, now: std::time::Instant) {
        if let Some(last) = self.last_monitor_refresh
            && now.duration_since(last)
                < std::time::Duration::from_secs(super::super::footer::MONITOR_REFRESH_SECS)
        {
            return;
        }
        self.last_monitor_refresh = Some(now);
        let Ok(Some(store)) = mur_monitor::store::MonitorStore::open_existing(&self.home) else {
            return;
        };
        let Ok(rows) = store.list(&mur_monitor::store::ListFilter::default()) else {
            return;
        };
        self.monitor_conditions = super::super::footer::conditions(&rows);
    }

    /// The in-flight agent bubble, if any. Searched from the back instead of
    /// only checking `last()`: a system note pushed mid-turn (HITL "approved
    /// `tool`", a hint, a warning) lands AFTER the streaming bubble, and the
    /// turn's deltas/finish must still find their message (see #6: approving a
    /// tool call used to lose the whole reply).
    pub(super) fn streaming_agent_mut(&mut self) -> Option<&mut ChatMsg> {
        self.messages
            .iter_mut()
            .rev()
            .find(|m| m.role == Role::Agent && m.streaming)
    }

    /// Append a turn to the session log, surfacing a write failure once.
    /// `suggested` carries the quick-reply options offered this turn (agent
    /// turns only) so they persist with the reply (#716).
    pub(super) fn persist_turn(
        &mut self,
        role: &str,
        text: &str,
        task_id: Option<&str>,
        suggested: &[super::super::suggest::Suggestion],
    ) {
        match self.session.append(role, text, task_id, suggested) {
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
    /// The `$PATH` executable list, scanned on first use.
    pub fn path_bins(&mut self) -> &[String] {
        self.path_bins
            .get_or_insert_with(super::super::shell_complete::scan_path_bins)
            .as_slice()
    }

    /// The session half of what the settings menus mark as in force.
    pub fn current_values(&self) -> super::super::complete::Current {
        super::super::complete::Current {
            session_effort: self.session_effort,
            auto: self.auto_approve,
            verbose: self.cards_expanded,
            skin: super::super::theme::skin_name(self.theme),
        }
    }

    pub fn input_text(&self) -> String {
        self.input.lines().join("\n")
    }

    pub fn clear_input(&mut self) {
        self.input = new_input();
    }

    /// Ingest a `Task.usage` JSON object: update per-turn and session counters
    /// and refresh `ctx_tokens` if the runtime emitted `context_tokens`.
    pub fn apply_usage(&mut self, usage: &serde_json::Value) {
        let u = super::super::footer::parse_usage(usage);
        self.turn_in = u.input;
        self.turn_out = u.output;
        self.session_in += u.input;
        self.session_out += u.output;
        if let Some(c) = super::super::footer::context_tokens(usage) {
            self.ctx_tokens = c;
        }
        // The runtime reports which model actually answered. Despite the field
        // name this is the provider's model NAME (`LlmResponse.model`), not a
        // registry key — the runtime writes `resp.model` into it.
        //
        // This arrived on every turn and was thrown away here, while the footer
        // priced the session from the configured `model_ref` looked up once at
        // startup. So whenever the fallback chain substituted a model, the
        // `$x est` figure was computed at the wrong rates and nothing said so.
        if let Some(answered) = usage.get("model_ref").and_then(|v| v.as_str()) {
            self.apply_answering_model(answered);
        }
    }

    /// Re-price from the model that actually answered, and say so once when it
    /// is not the configured one.
    fn apply_answering_model(&mut self, answered: &str) {
        let Some(book) = &self.pricing_book else {
            return;
        };
        let Some(key) = book.key_for_model(answered).map(str::to_string) else {
            // A model the registry has never heard of: we cannot price it, and
            // guessing at the configured model's rates is how the wrong number
            // got shown in the first place.
            self.pricing = super::super::footer::Pricing::default();
            return;
        };
        self.pricing = book.pricing_for_key(&key);
        let configured = book.configured_key.clone();
        // Announce only on a CHANGE, so a long session substituted once does
        // not repeat the notice every turn.
        if Some(&key) != configured.as_ref() && self.answered_key.as_ref() != Some(&key) {
            let from = configured.as_deref().unwrap_or("the configured model");
            self.push_system(format!(
                "⇄ answered by '{key}' ({answered}), not '{from}' — the runtime fell back. \
                 Cost below is now priced for '{key}'."
            ));
        }
        self.answered_key = Some(key);
    }

    /// Reveal suggestions captured this turn: one → ghost placeholder, many →
    /// completion overlay. No-op unless the composer is empty. Clears
    /// `pending_suggestions` either way.
    pub fn reveal_suggestions(&mut self) {
        let pending = std::mem::take(&mut self.pending_suggestions);
        let input_empty = self.input_text().is_empty();
        match super::super::suggest::plan_reveal(pending, input_empty) {
            super::super::suggest::Reveal::None => {}
            super::super::suggest::Reveal::Ghost(text) => {
                self.suggestion_ghost = Some(text.clone());
                self.input.set_placeholder_text(text);
            }
            super::super::suggest::Reveal::Chooser(items) => {
                let candidates: Vec<super::super::complete::Candidate> = items
                    .into_iter()
                    .map(|s| super::super::complete::Candidate {
                        display: s.text.clone(),
                        insert: s.text,
                        desc: s.desc.unwrap_or_default(),
                        has_children: false,
                    })
                    .collect();
                self.completion = Some(super::super::complete::CompletionState {
                    items: candidates,
                    selected: 0,
                    spaced: true,
                    current: None,
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

    /// Columns a message body may use at the current pane width — what a
    /// finished reply's markdown is rendered at (tables need it up front).
    pub fn body_cols(&self) -> usize {
        markdown::body_cols(self.width, self.theme.inner_padding)
    }

    /// Re-render every cached reply at the current width. A table decided
    /// its column widths when the reply finished; after a resize the pane is
    /// a different width and the cache would either overflow or leave a
    /// margin. Called from the resize rebuild, which replays the transcript
    /// from index 0 anyway, so the fresh render is what gets painted.
    pub fn rerender_markdown(&mut self) {
        let width = self.body_cols();
        for m in &mut self.messages {
            if m.rendered.is_some() {
                m.rendered = Some(markdown::render(&m.text, width).lines);
            }
        }
    }

    /// Does the welcome banner belong on screen? While no one has spoken — a
    /// slash command's notice is the UI talking, not a conversation — and this
    /// pane is not live-tailing someone else's channel. Consulted when the
    /// screen is (re)built: startup, `/clear`, a resize.
    pub fn welcome_applies(&self) -> bool {
        self.messages.iter().all(|m| m.role == Role::System) && self.follow.is_none()
    }

    /// The welcome banner lines for this pane, in this skin.
    pub fn welcome_banner(&self) -> Vec<ratatui::text::Line<'static>> {
        super::super::welcome::welcome_lines(
            self.theme,
            self.mascot_mode,
            &self.agent,
            self.cwd.as_deref(),
        )
    }
}
