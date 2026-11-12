//! murmur `/ssh`: the SSH setup card (SSH setup spec, "murmur", slice 4).
//!
//! The card borrows the approval panel's slot in the docked band; an open
//! HITL gate always outranks it. Everything that touches the network, the
//! profile or the runtime runs off the UI thread and reports back as a
//! `StreamMsg`: the plan (one `ssh-keyscan`), and `apply_and_restart`.

pub mod render;
pub mod state;

use std::path::Path;

use chrono::Utc;
use mur_ssh_request::RequestHost;
use mur_ssh_request::{PendingRequest, read_pending};
use tokio::sync::mpsc;

use super::app::{App, HandoverRequest};
use super::stream::StreamMsg;
use crate::cmd::agent::ssh::setup::life::AgentLifecycle;
use crate::cmd::agent::ssh::setup::plan::split_keys;
use crate::cmd::agent::ssh::setup::probe::System;
use crate::cmd::agent::ssh::setup::{Applied, Approval, apply_and_restart, plan};
use crate::cmd::agent::ssh::view;
use state::{CardState, KeyAction, Origin};

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

fn agent_home(app: &App) -> std::path::PathBuf {
    app.home.join("agents").join(&app.agent)
}

/// `/ssh` with its arguments.
pub fn command(app: &mut App, args: &[String], tx: &mpsc::Sender<StreamMsg>) {
    match args {
        [] => list(app, tx),
        [n] if n.parse::<usize>().is_ok() => {
            let pending = read_pending(&agent_home(app), Utc::now());
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
    match s.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => match p.parse() {
            Ok(n) => (h, Some(n)),
            Err(_) => (s, None),
        },
        _ => (s, None),
    }
}

/// Bare `/ssh`: the grants, their status, and numbered pending requests. A
/// single pending request opens straight away.
fn list(app: &mut App, tx: &mpsc::Sender<StreamMsg>) {
    let pending = read_pending(&agent_home(app), Utc::now());
    if pending.len() == 1 {
        return open(app, Origin::Pending(pending[0].clone()), tx);
    }
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
    let pending = read_pending(&agent_home(app), Utc::now());
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
    if app.ssh_card.is_some() || app.ssh_busy {
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
    let pending = read_pending(&agent_home(app), Utc::now());
    let Some(r) = pending.into_iter().find(|r| r.target == target) else {
        // Already allowed: the tool answered with the live state.
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
    if app.ssh_busy {
        return app.push_system("/ssh: still working on the last step");
    }
    app.ssh_busy = true;
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
    let (path, profile) =
        crate::cmd::agent::load_profile_for_edit(agent).map_err(|e| format!("{e:#}"))?;
    let agent_home = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.join("agents").join(agent));
    let p = plan(
        &System,
        agent,
        &agent_home,
        &profile.entitlements,
        target,
        user,
    )
    .map_err(|e| format!("{e:#}"))?;
    Ok(Box::new(CardState::new(p, origin)))
}

/// A terminal handover just ended (most often the card's own `ssh-add`, but
/// a `!ssh-add` typed by hand counts too): re-read which keys the ssh-agent
/// holds so the card stops calling a loaded key "not loaded".
pub fn after_handover(app: &mut App) {
    if let Some(card) = app.ssh_card.as_mut() {
        let (loaded, unloaded) = split_keys(&System);
        card.refresh_keys(loaded, unloaded);
    }
}

/// A key while the card is open.
pub fn on_key(app: &mut App, key: crossterm::event::KeyEvent, tx: &mpsc::Sender<StreamMsg>) {
    let Some(card) = app.ssh_card.as_mut() else {
        return;
    };
    match card.on_key(key) {
        KeyAction::Nothing => {}
        KeyAction::Close => {
            app.ssh_card = None;
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
    let Some(card) = app.ssh_card.take() else {
        return;
    };
    app.ssh_busy = true;
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
    let (_, profile) =
        crate::cmd::agent::load_profile_for_edit(agent).map_err(|e| format!("{e:#}"))?;
    let exe = std::env::current_exe()
        .map(super::multiplex::canonical_mur_exe)
        .map_err(|e| format!("could not locate the mur executable: {e}"))?;
    let mut life = AgentLifecycle::quiet(agent, home, exe);
    apply_and_restart(home, agent, profile.identity.key_version, a, &mut life)
        .map_err(|e| format!("{e:#}"))
}

/// A `CardMsg` arrived.
pub fn on_msg(app: &mut App, msg: CardMsg, tx: &mpsc::Sender<StreamMsg>) {
    app.ssh_busy = false;
    match msg {
        CardMsg::Planned(Ok(card)) => app.ssh_card = Some(card),
        CardMsg::Planned(Err(e)) => app.push_error(format!("/ssh: {e}")),
        CardMsg::Applied { result: Err(e), .. } => app.push_error(format!("/ssh: {e}")),
        CardMsg::Applied {
            result: Ok(applied),
            hand_back,
        } => {
            let ready = applied.restart == crate::cmd::agent::ssh::setup::apply::Restart::Restarted;
            for l in report(&app.agent, &applied) {
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

/// The lines after Approve, in order.
pub fn report(agent: &str, a: &Applied) -> Vec<String> {
    use crate::cmd::agent::ssh::edit::AllowOutcome;
    let mut out = vec![match a.outcome {
        AllowOutcome::Added => format!("✓ ssh access written for '{agent}'"),
        AllowOutcome::Updated => format!("✓ already allowed for '{agent}' (labels refreshed)"),
    }];
    if a.spawn.is_some() {
        out.push("`ssh` added to the spawn allowlist".into());
    }
    out.extend(a.preapproval.line());
    out.extend(a.restart.line(agent));
    out
}

#[cfg(test)]
mod tests;
