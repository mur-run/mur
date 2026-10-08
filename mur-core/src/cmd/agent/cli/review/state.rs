//! The attached `/review` session and the one entry point that drives it.

use mur_channel::ChannelService;
use tokio::sync::mpsc::Sender;

use super::args::{ReviewLine, parse_review_line};
use super::render::status_line;
use crate::cmd::agent::cli::ReviewEsc;
use crate::cmd::agent::cli::app::App;
use crate::cmd::agent::cli::stream::StreamMsg;
use crate::cmd::fleet::review::constants::REVIEW_ALREADY_ATTACHED;
use crate::cmd::fleet::review::murmur::worker::WorkerHandle;

/// What MURMUR holds while a review is attached (spec §3.4, §4).
pub struct ReviewSession {
    pub name: String,
    pub channel_id: String,
    pub handle: Option<WorkerHandle>,
    pub esc: ReviewEsc,
    /// Set by Ctrl+D while the worker is still finishing its turn (§4.4).
    pub closing: bool,
}

/// `/review <rest>` typed in the composer.
pub async fn handle(app: &mut App, raw: &str, _tx: &Sender<StreamMsg>) {
    if let Some(attached) = app.review.as_ref() {
        let text = match parse_review_line(raw) {
            Ok(ReviewLine::Bare) => attached_status(&app.home, attached),
            _ => REVIEW_ALREADY_ATTACHED.replace("{session}", &attached.name),
        };
        app.push_system(text);
    }
}

/// The status line from the channel as it stands now; the name alone if the
/// channel cannot be read (the line is informational, never a failure).
fn attached_status(home: &std::path::Path, s: &ReviewSession) -> String {
    let events = ChannelService::open(home)
        .and_then(|svc| svc.store().load_events(&s.channel_id))
        .unwrap_or_default();
    status_line(s, &events)
}
