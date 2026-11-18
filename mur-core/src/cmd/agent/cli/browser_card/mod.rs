//! murmur's browser request card (browser setup journey, slice 4).
//!
//! Opens when a streamed `browser_request` leaves a pending request, or from
//! `/browser requests`. It shares the approval panel's slot (an open HITL
//! gate and the SSH card both outrank it). Every read and write goes through
//! `request_card::front`, the same path as `mur browser answer` and Hub, and
//! runs off the UI thread. The card never opens a port (D1): it shows the
//! `--open-port` command for the user to run.

pub mod render;
pub mod state;

use std::path::{Path, PathBuf};

use mur_browser_request::Site;
use tokio::sync::{mpsc, oneshot};

use super::app::App;
use super::stream::StreamMsg;
use crate::cmd::browser::request_card::front::{
    self, Answered, PendingRequest, Pick, Settled, Shown,
};
use state::{KeyAction, Phase, State};

#[derive(Debug, Default)]
pub struct BrowserSlot {
    pub card: Option<Box<State>>,
    /// A read or settle is running off the UI thread.
    pub busy: bool,
    /// The streamed `browser_request` call, `(step_id, args)`.
    pub tool_step: Option<(String, serde_json::Value)>,
    /// The "logged in" action while the login browser is open. Dropping it
    /// cancels the login with nothing saved (D6).
    pub logged_in: Option<oneshot::Sender<()>>,
}

#[derive(Debug)]
pub enum CardMsg {
    Shown(Result<Box<Shown>, String>),
    /// A progress line from a running settle.
    Say(String),
    Settled(Result<Box<SettledCard>, String>),
}

#[derive(Debug)]
pub struct SettledCard {
    pub settled: Settled,
    /// The saved login's D4 label, read after the save.
    pub status: Option<front::Status>,
}

pub const USAGE: &str =
    "usage: /browser requests [site] — open the card for a pending browser request";

fn pending(app: &App) -> Vec<PendingRequest> {
    front::pending(&app.home, &app.agent)
}

pub fn pending_list(p: &[PendingRequest]) -> String {
    let mut s = String::from(
        "pending browser requests (the agent's own words) — /browser requests <site> opens one:\n",
    );
    for r in p {
        s.push_str(&format!("  {}  “{}”\n", r.site.origin(), r.reason));
    }
    s
}

/// On attach: name pending requests without opening anything.
pub fn attach_notice(app: &mut App) {
    let p = pending(app);
    if !p.is_empty() {
        app.push_system(pending_list(&p).trim_end().to_string());
    }
}

/// `/browser requests [site]`.
pub fn command(app: &mut App, args: &[String], tx: &mpsc::Sender<StreamMsg>) {
    let p = pending(app);
    let site = match args {
        [] => match p.as_slice() {
            [] => return app.push_system("no pending browser requests"),
            [only] => only.site.clone(),
            _ => return app.push_system(pending_list(&p).trim_end().to_string()),
        },
        [raw] => match Site::parse(raw) {
            Ok(s) => s,
            Err(e) => return app.push_error(format!("/browser requests: {e}")),
        },
        _ => return app.push_system(USAGE),
    };
    open(app, site, tx);
}

/// The streamed `browser_request` call finished: open its card if it left a
/// pending request.
pub fn on_tool_result(app: &mut App, args: &serde_json::Value, tx: &mpsc::Sender<StreamMsg>) {
    if app.browser.card.is_some() || app.browser.busy {
        return;
    }
    let Some(site) = args
        .get("site")
        .and_then(|v| v.as_str())
        .and_then(|s| Site::parse(s).ok())
    else {
        return;
    };
    if !pending(app).iter().any(|r| r.site == site) {
        return;
    }
    open(app, site, tx);
}

pub fn open(app: &mut App, site: Site, tx: &mpsc::Sender<StreamMsg>) {
    if app.browser.busy {
        return app.push_system("/browser: still working on the last step");
    }
    app.browser.busy = true;
    let (home, agent, tx) = (app.home.clone(), app.agent.clone(), tx.clone());
    tokio::spawn(async move {
        let res = tokio::task::spawn_blocking(move || read(&home, &agent, &site))
            .await
            .unwrap_or_else(|e| Err(format!("task failed: {e}")));
        let _ = tx.send(StreamMsg::BrowserCard(CardMsg::Shown(res))).await;
    });
}

fn read(home: &Path, agent: &str, site: &Site) -> Result<Box<Shown>, String> {
    front::show(home, agent, site)
        .map(Box::new)
        .map_err(|e| format!("{e:#}"))
}

/// A key while the card is open.
pub fn on_key(app: &mut App, key: crossterm::event::KeyEvent, tx: &mpsc::Sender<StreamMsg>) {
    let Some(card) = app.browser.card.as_mut() else {
        return;
    };
    match card.on_key(key) {
        KeyAction::Nothing => {}
        KeyAction::Close => close(app, None),
        KeyAction::LoggedIn => {
            if let Some(s) = app.browser.logged_in.take() {
                let _ = s.send(());
            }
            card.phase = Phase::Working;
        }
        KeyAction::CancelLogin => {
            // Dropping the sender is the cancel; the settle reports back.
            app.browser.logged_in = None;
            card.phase = Phase::Working;
        }
        KeyAction::Settle(pick) => {
            let (tx_in, rx_in) = oneshot::channel();
            card.phase = if matches!(pick, Pick::Login { .. }) {
                app.browser.logged_in = Some(tx_in);
                Phase::LoggingIn
            } else {
                drop(tx_in);
                Phase::Working
            };
            card.note = None;
            let shown = card.shown.clone();
            spawn_settle(app.home.clone(), shown, pick, rx_in, tx.clone());
        }
    }
}

fn spawn_settle(
    home: PathBuf,
    shown: Shown,
    pick: Pick,
    logged_in: oneshot::Receiver<()>,
    tx: mpsc::Sender<StreamMsg>,
) {
    // `settle` holds `&mut dyn` ops across awaits, so its future is not
    // `Send`: drive it on a blocking thread instead of `tokio::spawn`.
    let rt = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        rt.block_on(async move {
            let say_tx = tx.clone();
            let mut say = move |l: String| {
                let _ = say_tx.try_send(StreamMsg::BrowserCard(CardMsg::Say(l)));
            };
            let gate = async move {
                logged_in
                    .await
                    .map_err(|_| anyhow::anyhow!("the login was cancelled"))
            };
            let res = front::settle(&home, &shown, pick, gate, &mut say)
                .await
                .map(|settled| {
                    let status = settled
                        .saved_as
                        .as_deref()
                        .and_then(|p| front::status(&home, p).ok());
                    Box::new(SettledCard { settled, status })
                })
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(StreamMsg::BrowserCard(CardMsg::Settled(res))).await;
        })
    });
}

fn close(app: &mut App, line: Option<String>) {
    app.browser.card = None;
    app.browser.logged_in = None;
    app.push_system(line.unwrap_or_else(|| {
        "/browser: closed — the request is still pending; /browser requests reopens it".into()
    }));
}

pub fn on_msg(app: &mut App, msg: CardMsg, tx: &mpsc::Sender<StreamMsg>) {
    match msg {
        CardMsg::Say(l) => match app.browser.card.as_mut() {
            Some(c) => c.note = Some(l),
            None => app.push_system(l),
        },
        CardMsg::Shown(res) => {
            app.browser.busy = false;
            match res {
                Ok(shown) => {
                    app.browser.card = Some(Box::new(State::new(*shown)));
                }
                Err(e) => app.push_error(format!("/browser: {e}")),
            }
        }
        CardMsg::Settled(res) => {
            app.browser.logged_in = None;
            match res {
                Err(e) => {
                    // The request is kept; let the user try another row.
                    if let Some(c) = app.browser.card.as_mut() {
                        c.phase = Phase::Choosing;
                        c.note = Some(format!("✗ {e} — nothing changed, the request is kept"));
                    }
                }
                Ok(done) => settled(app, *done, tx),
            }
        }
    }
}

fn settled(app: &mut App, done: SettledCard, tx: &mpsc::Sender<StreamMsg>) {
    let SettledCard { settled, status } = done;
    let saved = settled.saved_as.clone();
    match settled.answered {
        Answered::Done {
            effect,
            notes,
            hand_back,
        } => {
            let mut out = notes;
            if let Some(e) = effect {
                out.push(e.applied_line().to_string());
            }
            match (saved, status, app.browser.card.as_mut()) {
                (Some(profile), Some(status), Some(c)) => {
                    // Keep the card up so the D4 label is seen; Enter closes.
                    c.phase = Phase::Saved { profile, status };
                    c.note = (!out.is_empty()).then(|| out.join("\n"));
                }
                _ => close(
                    app,
                    Some(if out.is_empty() {
                        "/browser: answered".into()
                    } else {
                        out.join("\n")
                    }),
                ),
            }
            // D5: only the answer's own hand-back goes to the agent.
            if let Some(text) = hand_back {
                if app.streaming {
                    app.drop_dead_turn();
                }
                super::turn::start_turn(app, text, tx);
            }
        }
        Answered::PortFailed {
            effect,
            notes,
            error,
        } => {
            // The binding was saved; the port grant was not (council N6).
            let mut out = notes;
            if let Some(e) = effect {
                out.push(e.applied_line().to_string());
            }
            out.push(format!(
                "✗ the port was not opened: {error} — the request is kept"
            ));
            close(app, Some(out.join("\n")));
        }
        Answered::Refused(why) => {
            if let Some(c) = app.browser.card.as_mut() {
                c.phase = Phase::Choosing;
                c.note = Some(match saved {
                    Some(p) => {
                        format!("✗ {why} — the login was saved as “{p}”, the request is kept")
                    }
                    None => format!("✗ {why} — nothing written, the request is kept"),
                });
            }
        }
        Answered::Stale => {
            // Re-read the card rather than retry against old entries.
            let site = app
                .browser
                .card
                .as_ref()
                .map(|c| c.shown.request.site.clone());
            app.browser.card = None;
            app.push_system(
                "/browser: the browser entry changed since the card was read — reading it again",
            );
            if let Some(site) = site {
                open(app, site, tx);
            }
        }
    }
}

#[cfg(test)]
mod tests;
