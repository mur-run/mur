//! The attached `/review` session and the one entry point that drives it.

use std::collections::BTreeSet;

use mur_channel::ChannelService;
use mur_common::channel::ChannelEvent;
use tokio::sync::mpsc::Sender;

use super::args::{ReviewLine, parse_review_line};
use super::render::{LabelFacts, paused_list, status_line};
use super::start::{resume, start};
use crate::cmd::agent::cli::ReviewEsc;
use crate::cmd::agent::cli::app::App;
use crate::cmd::agent::cli::stream::StreamMsg;
use crate::cmd::fleet::review::constants::{
    REVIEW_ALREADY_ATTACHED, REVIEW_AUTO_REFUSED, REVIEW_SLASH,
};
use crate::cmd::fleet::review::driver::SendAnswer;
use crate::cmd::fleet::review::murmur::bridge::Reply;
use crate::cmd::fleet::review::murmur::worker::WorkerHandle;
use crate::cmd::fleet::review::resume::{Resumable, list_paused};

/// What the attached session is waiting on the human for.
#[derive(Debug)]
pub enum Awaiting {
    /// §8 step 4: `prepare_resume` succeeded and holds the lock; the next
    /// line answers `Paused — continue?`. Dropping it releases the lock.
    ResumeConfirm(Box<Resumable>),
    /// §5: the worker's send gate; the next line answers it. Dropping
    /// `reply` reads as Stop on the worker side (§4.4).
    Confirm {
        open: BTreeSet<String>,
        reply: Reply<SendAnswer>,
    },
}

/// The send gate (answered by `answer_confirm`), not the resume question.
pub fn is_send_gate(a: &Awaiting) -> bool {
    matches!(a, Awaiting::Confirm { .. })
}

/// What MURMUR holds while a review is attached (spec §3.4, §4).
pub struct ReviewSession {
    pub name: String,
    pub channel_id: String,
    pub handle: Option<WorkerHandle>,
    pub esc: ReviewEsc,
    pub awaiting: Option<Awaiting>,
    /// Set by Ctrl+D while the worker is still finishing its turn (§4.4).
    pub closing: bool,
    /// AC-P3b-13: the `@<unknown>` hint cache for the send gate.
    pub hint: super::hint::InlineHint,
    /// The member turn Esc acts on (P3b-§6.2, §6.3).
    pub turn: super::keys::LiveTurn,
    /// §6.5: what the footer label is built from, read off the channel at
    /// attach and turn boundaries so the status bar never reads a file.
    pub label: super::render::LabelFacts,
}

/// `/review <rest>` typed in the composer. The composer was cleared on
/// submit; a refusal puts the typed line back (§3.3).
pub async fn handle(app: &mut App, raw: &str, tx: &Sender<StreamMsg>) {
    if let Some(attached) = app.review.as_ref() {
        let text = match parse_review_line(raw) {
            Ok(ReviewLine::Bare) => attached_status(&app.home, attached),
            _ => REVIEW_ALREADY_ATTACHED.replace("{session}", &attached.name),
        };
        app.push_system(text);
        return;
    }
    let started = match parse_review_line(raw) {
        Err(syntax) => Err(syntax),
        Ok(ReviewLine::AutoRefused) => Err(REVIEW_AUTO_REFUSED.to_string()),
        Ok(ReviewLine::Bare) => {
            match list_paused(&app.home) {
                Ok(rows) => app.push_system(paused_list(&rows)),
                Err(e) => app.push_error(format!("{e:#}")),
            }
            return;
        }
        Ok(ReviewLine::Start(args)) => start(&app.home, &args, tx).map_err(|e| format!("{e:#}")),
        Ok(ReviewLine::Resume(name)) => resume(&app.home, &name, tx).map_err(|e| format!("{e:#}")),
    };
    match started {
        Ok((session, text)) => {
            app.push_system(text);
            app.review = Some(session);
            refresh_label(app);
        }
        Err(refused) => {
            app.push_error(refused);
            app.set_input(&format!("/{REVIEW_SLASH} {}", raw.trim()));
        }
    }
}

/// The status line from the channel as it stands now; the name alone if the
/// channel cannot be read (the line is informational, never a failure).
fn attached_status(home: &std::path::Path, s: &ReviewSession) -> String {
    status_line(s, &channel_events(home, &s.channel_id))
}

/// Re-read the footer label's facts (§6.5). Called at attach and at turn
/// boundaries, so the status bar itself never touches the filesystem.
pub(super) fn refresh_label(app: &mut App) {
    let home = app.home.clone();
    if let Some(s) = app.review.as_mut() {
        s.label = LabelFacts::from_events(&channel_events(&home, &s.channel_id));
    }
}

/// The channel as it stands; empty if unreadable (display only).
fn channel_events(home: &std::path::Path, channel_id: &str) -> Vec<ChannelEvent> {
    ChannelService::open(home)
        .and_then(|svc| svc.store().load_events(channel_id))
        .unwrap_or_default()
}
