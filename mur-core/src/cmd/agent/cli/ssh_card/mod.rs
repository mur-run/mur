//! murmur `/ssh`: the SSH setup card (SSH setup spec, "murmur", slice 4).
//!
//! The card borrows the approval panel's slot in the docked band; an open
//! HITL gate always outranks it. Everything that touches the network, the
//! profile or the runtime runs off the UI thread and reports back as a
//! `StreamMsg`: the plan (one `ssh-keyscan`), and `apply_and_restart`.

pub mod render;
pub mod state;

use std::path::Path;

use mur_ssh_request::PendingRequest;
use mur_ssh_request::RequestHost;
use tokio::sync::mpsc;

use super::app::{App, HandoverRequest};
use super::stream::StreamMsg;
use crate::cmd::agent::ssh::setup::plan::split_keys;
use crate::cmd::agent::ssh::setup::probe::System;
use crate::cmd::agent::ssh::setup::{Applied, Approval, front, pending, report};
use crate::cmd::agent::ssh::view;
use state::{CardState, KeyAction, Origin};

/// Keys arriving this soon after a card opens are dropped: the card can open
/// by itself when a streamed `ssh_request` completes, and a user still typing
/// in the composer must not tick its consent or start an edit by accident.
pub const SETTLE: std::time::Duration = std::time::Duration::from_millis(800);

/// Everything `/ssh` keeps on the App.
#[derive(Debug, Default)]
pub struct SshSlot {
    /// The open card. Shares the approval panel's slot; an open HITL gate
    /// outranks it.
    pub card: Option<Box<CardState>>,
    /// When `card` was opened, for [`SETTLE`].
    pub opened_at: Option<std::time::Instant>,
    /// A plan or apply is running off the UI thread.
    pub busy: bool,
    /// The streamed `ssh_request` call, `(step_id, args)`, until it completes.
    pub tool_step: Option<(String, serde_json::Value)>,
}

impl SshSlot {
    /// Whether a key should reach the card now, rather than be dropped.
    pub fn settled(&self) -> bool {
        self.opened_at.is_none_or(|t| t.elapsed() >= SETTLE)
    }
}

pub const USAGE: &str =
    "usage: /ssh [host[:port]] [user] — set up SSH access; bare /ssh lists pending requests";

/// What the background plan or apply sends back.
#[derive(Debug)]
pub enum CardMsg {
    Planned(Result<Box<CardState>, String>),
    Applied {
        result: Result<Applied, String>,
        hand_back: Option<String>,
    },
}

/// `/ssh` with its arguments.
pub fn command(app: &mut App, args: &[String], tx: &mpsc::Sender<StreamMsg>) {
    match args {
        [] => list(app, tx),
        [n] if n.parse::<usize>().is_ok() => {
            let pending: Vec<_> = open_requests(app).into_iter().map(|o| o.request).collect();
            match n
                .parse::<usize>()
                .ok()
                .and_then(|i| pending.get(i.wrapping_sub(1)))
            {
                Some(r) => open(app, Origin::Pending(r.clone()), tx),
                None => app.push_error(format!("no pending request #{n}; /ssh lists them")),
            }
        }
        [host] | [host, _] => {
            let (h, port) = split_port(host);
            let target = match RequestHost::parse(h, port) {
                Ok(t) => t,
                Err(e) => return app.push_error(format!("/ssh: {e}")),
            };
            let user = args.get(1).cloned();
            spawn_plan(app, target, user, Origin::Direct, tx);
        }
        _ => app.push_system(USAGE),
    }
}

fn split_port(s: &str) -> (&str, Option<u16>) {
    // `[v6]:port`: the brackets are what make the port unambiguous.
    if s.starts_with('[')
        && let Some((h, p)) = s.rsplit_once("]:")
    {
        return match p.parse() {
            Ok(n) => (&s[..=h.len()], Some(n)),
            Err(_) => (s, None),
        };
    }
    match s.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => match p.parse() {
            Ok(n) => (h, Some(n)),
            Err(_) => (s, None),
        },
        _ => (s, None),
    }
}

fn open_requests(app: &App) -> Vec<pending::OpenRequest> {
    pending::open_requests(&app.home, &app.agent)
}

/// Bare `/ssh`: the grants, their status, and numbered pending requests. A
/// single open request for a host with no live grant opens straight away.
fn list(app: &mut App, tx: &mpsc::Sender<StreamMsg>) {
    let open_reqs = open_requests(app);
    if let [only] = open_reqs.as_slice()
        && !only.granted
    {
        return open(app, Origin::Pending(only.request.clone()), tx);
    }
    let pending: Vec<_> = open_reqs.into_iter().map(|o| o.request).collect();
    let mut out = match crate::cmd::agent::load_profile_for_edit(&app.agent) {
        Ok((_, p)) => {
            let ssh = &p.entitlements.ssh;
            let st = crate::cmd::agent::ssh::status::status(&app.home, &app.agent, ssh);
            format!(
                "{}{}",
                view::render(&app.agent, ssh),
                view::render_status(&st)
            )
        }
        Err(e) => format!("could not read the profile: {e:#}\n"),
    };
    if pending.is_empty() {
        out.push_str("no pending requests · /ssh <host> sets one up");
    } else {
        out.push_str(&pending_list(&pending));
    }
    app.push_system(out.trim_end().to_string());
}

/// On attach: say there are pending requests, without opening a card or
/// scanning anything (the scan runs only when a card opens).
pub fn attach_notice(app: &mut App) {
    let pending: Vec<_> = open_requests(app)
        .into_iter()
        .filter(|o| !o.granted)
        .map(|o| o.request)
        .collect();
    if !pending.is_empty() {
        app.push_system(pending_list(&pending).trim_end().to_string());
    }
}

pub fn pending_list(pending: &[PendingRequest]) -> String {
    let mut s = String::from("pending requests (the agent's own words) — /ssh <N> opens one:\n");
    for (i, r) in pending.iter().enumerate() {
        s.push_str(&format!(
            "  {}. {}  “{}”\n",
            i + 1,
            r.target.canonical(),
            r.reason
        ));
    }
    s
}

pub fn open(app: &mut App, origin: Origin, tx: &mpsc::Sender<StreamMsg>) {
    let Some(r) = origin.request().cloned() else {
        return;
    };
    spawn_plan(app, r.target, r.user, origin, tx);
}

/// The streamed `ssh_request` call finished: if it left a pending request,
/// open its card with the triggering user text as the resend.
pub fn on_tool_result(app: &mut App, args: &serde_json::Value, tx: &mpsc::Sender<StreamMsg>) {
    if app.ssh.card.is_some() || app.ssh.busy {
        return;
    }
    let Some(host) = args.get("host").and_then(|v| v.as_str()) else {
        return;
    };
    let port = args
        .get("port")
        .and_then(|v| v.as_u64())
        .and_then(|p| u16::try_from(p).ok());
    let Ok(target) = RequestHost::parse(host, port) else {
        return;
    };
    let Some(r) = open_requests(app)
        .into_iter()
        .find(|o| o.request.target == target && !o.granted)
        .map(|o| o.request)
    else {
        // Already allowed (or cleared): the tool answered with the live state.
        return;
    };
    let origin = Origin::Tool {
        request: r,
        resend: app.last_sent.clone(),
    };
    open(app, origin, tx);
}

fn spawn_plan(
    app: &mut App,
    target: RequestHost,
    user: Option<String>,
    origin: Origin,
    tx: &mpsc::Sender<StreamMsg>,
) {
    if app.ssh.busy {
        return app.push_system("/ssh: still working on the last step");
    }
    app.ssh.busy = true;
    app.push_system(format!(
        "scanning {} for its host keys…",
        target.canonical()
    ));
    let (agent, home, tx) = (app.agent.clone(), app.home.clone(), tx.clone());
    tokio::spawn(async move {
        let res = tokio::task::spawn_blocking(move || build(&home, &agent, target, user, origin))
            .await
            .unwrap_or_else(|e| Err(format!("task failed: {e}")));
        let _ = tx.send(StreamMsg::SshCard(CardMsg::Planned(res))).await;
    });
}

fn build(
    home: &Path,
    agent: &str,
    target: RequestHost,
    user: Option<String>,
    origin: Origin,
) -> Result<Box<CardState>, String> {
    front::plan_for(home, agent, target, user).map(|p| Box::new(CardState::new(p, origin)))
}

/// A terminal handover just ended (the card's own `ssh-add`): re-read which
/// keys the ssh-agent holds. The card moves on to the host step only if the
/// chosen key is now loaded; a failed `ssh-add` leaves it on the key step.
pub fn after_handover(app: &mut App) {
    // Only the card's own `ssh-add` changes what the ssh-agent holds; any
    // other handover would re-run `ssh-add -L` on the UI thread for nothing.
    if let Some(card) = app.ssh.card.as_mut().filter(|c| c.loading) {
        let (loaded, unloaded) = split_keys(&System);
        card.refresh_keys(loaded, unloaded);
    }
}

/// A key while the card is open.
pub fn on_key(app: &mut App, key: crossterm::event::KeyEvent, tx: &mpsc::Sender<StreamMsg>) {
    let Some(card) = app.ssh.card.as_mut() else {
        return;
    };
    match card.on_key(key) {
        KeyAction::Nothing => {}
        KeyAction::Close => {
            app.ssh.card = None;
            app.push_system("/ssh: cancelled — nothing was changed");
        }
        KeyAction::LoadKey(path) => {
            app.pending_handover = Some(HandoverRequest {
                pre: Vec::new(),
                argv: vec![
                    crate::cmd::agent::ssh::resolve::SSH_ADD_BIN.to_string(),
                    path.display().to_string(),
                ],
                label: "ssh-add".into(),
                done_note: Some("ssh-add: key loaded; the card is still open".into()),
                _lock: None,
            });
        }
        KeyAction::Approve(a) => approve(app, a, tx),
    }
}

fn approve(app: &mut App, a: Approval, tx: &mpsc::Sender<StreamMsg>) {
    let Some(card) = app.ssh.card.take() else {
        return;
    };
    app.ssh.busy = true;
    app.push_system(format!(
        "/ssh: writing the grant for {} and restarting '{}'…",
        card.plan.host.canonical(),
        app.agent
    ));
    let hand_back = card.origin.hand_back();
    let (agent, home, tx) = (app.agent.clone(), app.home.clone(), tx.clone());
    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || run_apply(&home, &agent, &a))
            .await
            .unwrap_or_else(|e| Err(format!("task failed: {e}")));
        let _ = tx
            .send(StreamMsg::SshCard(CardMsg::Applied { result, hand_back }))
            .await;
    });
}

fn run_apply(home: &Path, agent: &str, a: &Approval) -> Result<Applied, String> {
    let exe = std::env::current_exe()
        .map(super::multiplex::canonical_mur_exe)
        .map_err(|e| format!("could not locate the mur executable: {e}"))?;
    front::apply_for(home, agent, a, exe)
}

/// A `CardMsg` arrived.
pub fn on_msg(app: &mut App, msg: CardMsg, tx: &mpsc::Sender<StreamMsg>) {
    app.ssh.busy = false;
    match msg {
        CardMsg::Planned(Ok(card)) => {
            app.ssh.card = Some(card);
            app.ssh.opened_at = Some(std::time::Instant::now());
        }
        CardMsg::Planned(Err(e)) => app.push_error(format!("/ssh: {e}")),
        CardMsg::Applied { result: Err(e), .. } => app.push_error(format!("/ssh: {e}")),
        CardMsg::Applied {
            result: Ok(applied),
            hand_back,
        } => {
            let ready = applied.restart == crate::cmd::agent::ssh::setup::apply::Restart::Restarted;
            for l in report::lines(&app.agent, &applied) {
                app.push_system(l);
            }
            // Spec "Error handling": a runtime that did not come up gets no
            // hand-back; the card already said why.
            if ready && let Some(text) = hand_back {
                // The restart ended any turn still in flight.
                if app.streaming {
                    app.drop_dead_turn();
                }
                super::turn::start_turn(app, text, tx);
            }
        }
    }
}

#[cfg(test)]
mod tests;
