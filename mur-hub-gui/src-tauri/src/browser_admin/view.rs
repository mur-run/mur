//! What the WebView sees for a browser request card. Display data only:
//! every browser rule ran in `mur_core::cmd::browser::request_card::front`.

use mur_core::cmd::browser::request_card::front::{
    Answered, Card, Effect, LoginAgain, Offer, PendingRequest, ProfileRow, Reach, Settled, Shown,
    Status, port_step,
};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct PendingView {
    pub agent: String,
    /// `scheme://host[:port]`, the key `agent_browser_card` takes back.
    pub site: String,
    pub login: bool,
    /// The agent's own words, already sanitized by the request reader.
    pub reason: String,
    pub requested_at: String,
}

pub fn pending_view(agent: &str, r: &PendingRequest) -> PendingView {
    PendingView {
        agent: agent.to_owned(),
        site: r.site.origin(),
        login: r.login,
        reason: r.reason.clone(),
        requested_at: r.requested_at.to_rfc3339(),
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OfferView {
    AlreadyBound,
    Offered,
    AddsHosts {
        missing: Vec<String>,
    },
    /// Usable only with the explicit replace choice (`replace: true`).
    ReplaceOnly {
        with: String,
        drops: Vec<String>,
        adds: Vec<String>,
    },
    Disabled {
        why: String,
    },
}

#[derive(Debug, Serialize)]
pub struct RowView {
    pub name: String,
    pub domains: Vec<String>,
    pub shared_with: Vec<String>,
    pub offer: OfferView,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReachView {
    Allowed,
    NeedsHost { missing: Vec<String> },
    Unscoped { mode: String },
    Disabled { why: String },
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BodyView {
    NoLiveEntry,
    Login {
        rows: Vec<RowView>,
        /// The profile "log in again" saves under; `exists` means it
        /// replaces a saved login of that name.
        login_again: String,
        login_again_exists: bool,
    },
    Reach {
        reach: ReachView,
    },
}

/// D1: the ANY-host sentence, and one complete CLI line per answer choice.
/// Each `commands` entry is what the copy button copies.
#[derive(Debug, Serialize)]
pub struct PortStepView {
    pub port: u16,
    pub warning: String,
    pub commands: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct CardView {
    pub id: String,
    pub agent: String,
    pub site: String,
    pub login: bool,
    pub reason: String,
    pub body: BodyView,
    /// D1: the port step is CLI-only; this is what to show for it.
    pub port_step: Option<PortStepView>,
}

fn offer_view(o: &Offer) -> OfferView {
    match o {
        Offer::AlreadyBound => OfferView::AlreadyBound,
        Offer::Offered => OfferView::Offered,
        Offer::AddsHosts { missing } => OfferView::AddsHosts {
            missing: missing.clone(),
        },
        Offer::ReplaceOnly {
            with, drops, adds, ..
        } => OfferView::ReplaceOnly {
            with: with.clone(),
            drops: drops.clone(),
            adds: adds.clone(),
        },
        Offer::Disabled { why } => OfferView::Disabled { why: why.clone() },
    }
}

fn row_view(r: &ProfileRow) -> RowView {
    RowView {
        name: r.name.clone(),
        domains: r.domains.clone(),
        shared_with: r.shared_with.clone(),
        offer: offer_view(&r.offer),
    }
}

fn reach_view(r: &Reach) -> ReachView {
    match r {
        Reach::Allowed => ReachView::Allowed,
        Reach::NeedsHost { missing } => ReachView::NeedsHost {
            missing: missing.clone(),
        },
        Reach::Unscoped { mode } => ReachView::Unscoped { mode: mode.clone() },
        Reach::Disabled { why } => ReachView::Disabled { why: why.clone() },
    }
}

pub fn card_view(id: &str, s: &Shown) -> CardView {
    let body = match &s.card {
        Card::NoLiveEntry => BodyView::NoLiveEntry,
        Card::Login {
            rows,
            login_again: LoginAgain { name, exists },
            ..
        } => BodyView::Login {
            rows: rows.iter().map(row_view).collect(),
            login_again: name.clone(),
            login_again_exists: *exists,
        },
        Card::Reach { reach, .. } => BodyView::Reach {
            reach: reach_view(reach),
        },
    };
    CardView {
        id: id.to_owned(),
        agent: s.agent.clone(),
        site: s.request.site.origin(),
        login: s.request.login,
        reason: s.request.reason.clone(),
        body,
        port_step: port_step(s).map(|p| PortStepView {
            port: p.port,
            warning: p.warning,
            commands: p.commands,
        }),
    }
}

#[derive(Debug, Serialize)]
pub struct AnsweredView {
    /// `done` | `port_failed` | `refused` | `stale`.
    pub state: &'static str,
    pub lines: Vec<String>,
    /// The login this answer saved first, if any.
    pub saved_as: Option<String>,
    /// D4 label for `saved_as`: `saved_not_tried` | `agent_reported`.
    pub status: Option<&'static str>,
    /// RFC 3339, with `agent_reported`.
    pub reported_at: Option<String>,
    /// D5: sent to the agent through the chat by the UI, only on `done`.
    pub hand_back: Option<String>,
    /// The request is still open (every state but `done`).
    pub kept: bool,
}

pub fn answered_view(s: Settled, status: Option<Status>) -> AnsweredView {
    let (label, at) = match status {
        Some(Status::SavedNotTried) => (Some("saved_not_tried"), None),
        Some(Status::AgentReported(t)) => (Some("agent_reported"), Some(t.to_rfc3339())),
        None => (None, None),
    };
    let effect_lines = |notes: Vec<String>, effect: Option<Effect>| {
        let mut out = notes;
        if let Some(e) = effect {
            out.push(Effect::applied_line(e).to_string());
        }
        out
    };
    let (state, lines, hand_back) = match s.answered {
        Answered::Done {
            effect,
            notes,
            hand_back,
        } => ("done", effect_lines(notes, effect), hand_back),
        Answered::PortFailed {
            effect,
            notes,
            error,
        } => {
            let mut l = effect_lines(notes, effect);
            l.push(format!("the port was not opened: {error}"));
            ("port_failed", l, None)
        }
        Answered::Refused(why) => ("refused", vec![why], None),
        Answered::Stale => (
            "stale",
            vec!["the browser entry changed since the card was read; open it again".into()],
            None,
        ),
    };
    AnsweredView {
        state,
        lines,
        saved_as: s.saved_as,
        status: label,
        reported_at: at,
        hand_back,
        kept: state != "done",
    }
}
