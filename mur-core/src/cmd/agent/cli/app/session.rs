//! Session, channel, follow and shell-card methods on `App`, moved out of `app/mod.rs` for CLAUDE.md §4's 800-line rule.
//! Pure movement: every item below is verbatim.

use super::*;

impl App {
    /// Reset to a brand-new conversation (drops server-side context). Any
    /// in-flight turn must already have been cancelled by the caller.
    pub fn start_new_session(&mut self, session: Session) {
        self.session = session;
        self.channel = None;
        self.messages.clear();
        self.flushed_upto = 0;
        self.flushed_bytes = 0;
        self.needs_full_redraw = true;
        self.context_task_id = None;
        self.current_task_id = None;
        self.streaming = false;
        self.hitl = None;
        self.hitl_queue.clear();
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
        let turns = super::super::persist::load(&self.home, channel_id, &self.agent)?;
        self.session = session;
        self.channel = None;
        self.messages.clear();
        self.flushed_upto = 0;
        self.flushed_bytes = 0;
        self.context_task_id = None;
        self.current_task_id = None;
        self.streaming = false;
        self.hitl = None;
        self.hitl_queue.clear();
        self.wants_screen_wipe = true;
        self.load_history(turns);
        self.refresh_channel();
        Ok(())
    }

    /// Begin live-tailing `channel_id` (`/channels N --follow`). Refuses this
    /// pane's own channel: its turns are already in the transcript and the
    /// tail would double-render every one of them.
    pub fn start_follow(
        &mut self,
        channel_id: &str,
        now: std::time::Instant,
    ) -> anyhow::Result<()> {
        if self.channel.as_ref().is_some_and(|c| c.id == channel_id) {
            anyhow::bail!("that is the channel this pane is on — nothing to follow");
        }
        self.follow = Some(super::super::follow::Follow::start(
            &self.home, channel_id, now,
        )?);
        Ok(())
    }

    /// Render whatever landed in the followed channel. Any error stops
    /// following — a vanished channel must not report itself every poll.
    pub fn poll_follow(&mut self, now: std::time::Instant) {
        let Some(mut f) = self.follow.take() else {
            return;
        };
        match f.drain(&self.home, now) {
            Ok(lines) => {
                for l in lines {
                    self.push_system(l);
                }
                self.follow = Some(f);
            }
            Err(e) => self.push_system(format!("follow {} stopped: {e:#}", f.tag())),
        }
    }

    /// Auto-arm live fleet progress when a delegated `fleet_run` step starts:
    /// the member rail (replaced only when absent or watching a different
    /// fleet) plus a milestone follow of `fleet-<name>`. A user-armed follow
    /// is never clobbered — the auto follow only takes an empty slot — and
    /// the pane's own channel is never followed (its turns already render).
    pub fn arm_auto_fleet(&mut self, step_id: &str, fleet: &str, now: std::time::Instant) {
        self.auto_fleet_step = Some(step_id.to_string());
        if self.fleet.as_ref().is_none_or(|r| r.fleet() != fleet) {
            self.fleet = Some(super::super::fleet_rail::FleetRail::start_auto(fleet));
        }
        if let Some(rail) = self.fleet.as_mut() {
            rail.set_run_in_flight(&self.home, true);
        }
        let channel_id = format!("fleet-{fleet}");
        let on_own_pane = self.channel.as_ref().is_some_and(|c| c.id == channel_id);
        let can_follow = self.follow.is_none() && !on_own_pane;
        if can_follow {
            self.follow = Some(super::super::follow::Follow::start_auto(
                &self.home,
                &channel_id,
                now,
            ));
        }
        self.push_system(if can_follow {
            format!(
                "⛴ fleet {fleet} started — following {channel_id} (milestones land here; /channels N --follow for the raw log)"
            )
        } else {
            format!("⛴ fleet {fleet} started")
        });
    }

    /// Close out an auto-armed `fleet_run` when its step completes: drain the
    /// follow's tail, drop it (only if auto), commit the rail's final member
    /// states to the transcript, and retire an auto-armed rail.
    ///
    /// The rail is live state repainted every frame and never flushed to
    /// scrollback, so leaving it up after the run kept the outcome visible
    /// but out of the record: `Ctrl+O` and the persisted transcript had
    /// nothing, and the band stayed on screen for the rest of the session
    /// showing a run that had already ended. Folding the view into the
    /// outcome message fixes both. A `--fleet` rail the user armed themselves
    /// stays up — they asked for a band, not a run report.
    pub fn finish_auto_fleet(&mut self, step_id: &str, ok: bool, duration_ms: u64) {
        if self.auto_fleet_step.as_deref() != Some(step_id) {
            return;
        }
        self.auto_fleet_step = None;
        // Catch the run's last events before the follow goes away.
        self.poll_follow(std::time::Instant::now());
        if self.follow.as_ref().is_some_and(|f| f.auto) {
            self.follow = None;
        }
        let home = self.home.clone();
        let mut fleet = String::new();
        let mut summary = Vec::new();
        let mut retire = false;
        let mut stop_word: Option<String> = None;
        if let Some(rail) = self.fleet.as_mut() {
            rail.set_run_in_flight(&home, false);
            // A view up to POLL_INTERVAL stale would freeze the wrong states
            // into history. `set_run_in_flight` already busted the poll gate.
            rail.poll(&home, std::time::Instant::now());
            fleet = rail.fleet().to_string();
            summary = rail.view().summary();
            retire = rail.is_auto();
            stop_word = rail
                .view()
                .stop
                .as_ref()
                .filter(|s| s.reason != "converged")
                .map(|s| s.reason.clone());
        }
        if retire {
            self.fleet = None;
        }
        let took =
            super::super::follow::fmt_elapsed(chrono::Duration::milliseconds(duration_ms as i64));
        // "finished" is reserved for a run that converged. A cap, a kill or an
        // unanswered approval is a stop, and the first line says so.
        let verdict = match (&stop_word, ok) {
            (Some(reason), _) => format!("stopped: {reason}"),
            (None, true) => "finished".to_string(),
            (None, false) => "failed".to_string(),
        };
        let head = format!("⛴ fleet {fleet} {verdict} ({took})");
        self.push_system(
            std::iter::once(head)
                .chain(summary)
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }

    /// Open the live card for a `!cmd` that was just accepted. The card
    /// exists from the keypress (D2), so a silent command is still visibly
    /// running, and `append_shell_output` always has a target.
    pub fn begin_shell(&mut self, cmd: &str) {
        let mut m = ChatMsg::new(Role::Shell, format!("$ {cmd}"));
        m.streaming = true;
        self.messages.push(m);
        self.scroll_back = 0;
    }

    /// The live shell card, if one is open.
    fn streaming_shell_mut(&mut self) -> Option<&mut ChatMsg> {
        self.messages
            .iter_mut()
            .rev()
            .find(|m| m.role == Role::Shell && m.streaming)
    }

    /// Append streamed `!cmd` output to the live card (D2), head-dropping
    /// past `SHELL_CARD_MAX_BYTES` so a chatty command cannot grow the
    /// transcript without bound (D6).
    ///
    /// NB: does not reset `scroll_back` — same reason as `append_delta`. A
    /// user scrolled up to read earlier output must not be yanked back to
    /// the bottom by every new line.
    pub fn append_shell_output(&mut self, chunk: &str) {
        let Some(m) = self.streaming_shell_mut() else {
            return; // no live card (a teardown cleared it); drop the chunk
        };
        if !m.text.is_empty() && !m.text.ends_with('\n') {
            m.text.push('\n');
        }
        m.text.push_str(chunk);
        if m.text.len() > super::super::shell::SHELL_CARD_MAX_BYTES {
            // The `$ cmd` line is the card's identity; keep it above the
            // truncation marker rather than letting the tail eat it.
            let first = m.text.lines().next().unwrap_or_default().to_string();
            let rest = m.text.split_once('\n').map(|(_, r)| r).unwrap_or_default();
            let kept =
                super::super::shell::cap_tail(rest, super::super::shell::SHELL_CARD_MAX_BYTES);
            m.text = format!("{first}\n{kept}");
        }
    }

    /// Close the live `!cmd` card: stamp how it ended, stop the spinner,
    /// persist it, and hand back the output body for the agent block.
    pub fn finish_shell(&mut self, end: &super::super::shell::ShellEnd) -> String {
        let Some(m) = self.streaming_shell_mut() else {
            return String::new();
        };
        if let Some(tail) = end.card_tail() {
            if !m.text.ends_with('\n') {
                m.text.push('\n');
            }
            m.text.push_str(&tail);
        }
        m.streaming = false;
        let text = m.text.trim_end().to_string();
        m.text = text.clone();
        self.persist_turn("shell", &text, None, &[]);
        // The block wants the output alone; the card's first line is `$ cmd`
        // and `shell_block` re-adds it.
        text.split_once('\n')
            .map(|(_, r)| r.to_string())
            .unwrap_or_default()
    }
}
