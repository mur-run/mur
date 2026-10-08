//! Text for the attached `/review` session (spec §3.4, §6.5).

use mur_common::channel::ChannelEvent;

use super::state::ReviewSession;
use crate::cmd::fleet::review::schema::{
    MICROS_PER_USD, NoteClassification, ReviewPayload, Role, classify_note_payload,
};
use crate::cmd::fleet::review::state::cumulative_of;

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
