//! Text for the attached `/review` session (spec §3.4, §6.5).

use chrono::{DateTime, Utc};
use mur_common::channel::ChannelEvent;

use super::state::ReviewSession;
use crate::cmd::agent::cli::app::App;
use crate::cmd::agent::cli::follow::fmt_elapsed;
use crate::cmd::fleet::review::constants::{
    REVIEW_FOOTER_HINT, REVIEW_LEFT_PAUSED_NOTICE, REVIEW_NO_PAUSED, REVIEW_ROW_CRASHED,
    REVIEW_ROW_NO_LAST, REVIEW_ROW_PAUSED, REVIEW_ROW_RESUMABLE, REVIEW_ROW_RUNNING,
    REVIEW_ROW_TIME_FORMAT, REVIEW_USAGE_MURMUR,
};
use crate::cmd::fleet::review::murmur::bridge::Outcome;
use crate::cmd::fleet::review::resume::PausedRow;
use crate::cmd::fleet::review::schema::{
    MICROS_PER_USD, NoteClassification, ReviewPayload, Role, classify_note_payload,
};
use crate::cmd::fleet::review::session::render_stop_screen;
use crate::cmd::fleet::review::state::{SessionState, cumulative_of};

/// Bare `/review` while attached: name, who the last send went to, and the
/// cost lower bound. Built only from events already on the channel (§6.5), so
/// the cost is `≥`: aborted turns are never ledgered.
pub fn status_line(s: &ReviewSession, events: &[ChannelEvent]) -> String {
    let payloads: Vec<ReviewPayload> = events
        .iter()
        .filter_map(|ev| match classify_note_payload(&ev.payload) {
            NoteClassification::Review(env) => Some(env.payload),
            _ => None,
        })
        .collect();
    let to = payloads.iter().rev().find_map(|p| match p {
        ReviewPayload::TurnSent { to, .. } => Some(*to),
        _ => None,
    });
    let micros = payloads
        .iter()
        .filter_map(cumulative_of)
        .map(|c| c.cost_usd_micros)
        .max()
        .unwrap_or(0);
    let mut out = format!("review {}", s.name);
    match to {
        Some(Role::Main) => out.push_str(" · →main"),
        Some(Role::Reviewer) => out.push_str(" · →reviewer"),
        None => {}
    }
    out.push_str(&format!(" · ≥ ${:.2}", micros as f64 / MICROS_PER_USD));
    out
}

/// Bare `/review` with nothing attached (§3.1, §3.4): usage, then one row per
/// session. Paused and crashed rows carry their resume command; a session
/// another process holds is `running` and never offered (AC-P3b-4b).
pub fn paused_list(rows: &[PausedRow]) -> String {
    let mut out = String::from(REVIEW_USAGE_MURMUR);
    out.push('\n');
    if rows.is_empty() {
        out.push('\n');
        out.push_str(REVIEW_NO_PAUSED);
    }
    for r in rows {
        out.push('\n');
        out.push_str(&paused_row(r));
    }
    out
}

fn paused_row(r: &PausedRow) -> String {
    let state = match &r.state {
        SessionState::Running(who) => {
            let who = who
                .as_deref()
                .map(|w| format!(" ({w})"))
                .unwrap_or_default();
            return REVIEW_ROW_RUNNING
                .replace("{session}", &r.name)
                .replace("{who}", &who);
        }
        SessionState::Crashed => REVIEW_ROW_CRASHED,
        // `list_paused` yields only Running, Paused and Crashed.
        _ => REVIEW_ROW_PAUSED,
    };
    let last = r.last.map_or_else(
        || REVIEW_ROW_NO_LAST.to_string(),
        |t| t.format(REVIEW_ROW_TIME_FORMAT).to_string(),
    );
    REVIEW_ROW_RESUMABLE
        .replace("{session}", &r.name)
        .replace("{state}", state)
        .replace("{last}", &last)
}

/// §7.2: how the worker's end reads in the transcript, as stdin prints it.
pub fn finished_block(o: &Outcome) -> String {
    match o {
        Outcome::Ran(stop, ledger, channel_id) => render_stop_screen(stop, ledger, channel_id),
        Outcome::LeftPaused => REVIEW_LEFT_PAUSED_NOTICE.to_string(),
        Outcome::Err(e) => e.clone(),
    }
}

/// §6.5: the footer label's inputs, folded from `turn_sent` and the
/// cumulative usage already on the channel. No new event kind.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LabelFacts {
    /// When the first `turn_sent` was ledgered; `None` before any send.
    pub first_sent: Option<DateTime<Utc>>,
    /// Highest cumulative cost seen. A lower bound: aborted turns are never
    /// ledgered (§13).
    pub cost_micros: u64,
}

impl LabelFacts {
    pub fn from_events(events: &[ChannelEvent]) -> Self {
        let mut facts = Self::default();
        for ev in events {
            let NoteClassification::Review(env) = classify_note_payload(&ev.payload) else {
                continue;
            };
            if matches!(env.payload, ReviewPayload::TurnSent { .. }) {
                facts.first_sent = Some(facts.first_sent.map_or(ev.ts, |t| t.min(ev.ts)));
            }
            if let Some(c) = cumulative_of(&env.payload) {
                facts.cost_micros = facts.cost_micros.max(c.cost_usd_micros);
            }
        }
        facts
    }
}

/// `review <name> · <elapsed> · ≥ $x`; the elapsed part only once a turn
/// has been sent.
pub fn review_label(name: &str, facts: &LabelFacts, now: DateTime<Utc>) -> String {
    let mut out = format!("review {name}");
    if let Some(t0) = facts.first_sent {
        out.push_str(&format!(
            " · {}",
            fmt_elapsed(now.signed_duration_since(t0))
        ));
    }
    out.push_str(&format!(
        " · ≥ ${:.2}",
        facts.cost_micros as f64 / MICROS_PER_USD
    ));
    out
}

/// The status bar's right hint for an attached session (AC-P3b-23): the
/// urgent review state first, else the plain Esc hint. `None` when detached.
pub fn footer_right_hint(app: &App) -> Option<&'static str> {
    app.review.as_ref()?;
    Some(super::keys::footer_hint(app).unwrap_or(REVIEW_FOOTER_HINT))
}

/// The status bar's left label for an attached session (§6.5).
pub fn footer_label(app: &App) -> Option<String> {
    let s = app.review.as_ref()?;
    Some(review_label(&s.name, &s.label, Utc::now()))
}
