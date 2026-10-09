//! Text for the attached `/review` session (spec §3.4, §6.5).

use mur_common::channel::ChannelEvent;

use super::state::ReviewSession;
use crate::cmd::fleet::review::constants::{
    REVIEW_NO_PAUSED, REVIEW_ROW_CRASHED, REVIEW_ROW_NO_LAST, REVIEW_ROW_PAUSED,
    REVIEW_ROW_RESUMABLE, REVIEW_ROW_RUNNING, REVIEW_ROW_TIME_FORMAT, REVIEW_USAGE_MURMUR,
};
use crate::cmd::fleet::review::resume::PausedRow;
use crate::cmd::fleet::review::schema::{
    MICROS_PER_USD, NoteClassification, ReviewPayload, Role, classify_note_payload,
};
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
