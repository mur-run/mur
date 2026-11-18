//! Hub's browser request card (#2360 s4). Every command is a thin call to
//! `mur_core::cmd::browser::request_card::front`; no browser rule lives here
//! or in TypeScript. A read card stays in this process under an opaque id,
//! so an answer applies against exactly the entries the card was built from.

mod view;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use mur_browser_request::Site;
use mur_core::a2a_dial::canonicalize_agent_name;
use mur_core::cmd::browser::request_card::front::{self, Pick, Shown};
use serde::Deserialize;
use tauri::State;
use tokio::sync::oneshot;

pub use view::{AnsweredView, CardView, PendingView};

/// Emitted when an agent's `browser_request` tool call completes in a Hub
/// chat, or a one-shot turn changed its open requests. Payload: agent name.
pub const BROWSER_REQUEST_EVENT: &str = "browser-request-written";

/// How long a read card stays usable before it must be read again.
pub(crate) const CARD_TTL: Duration = Duration::from_secs(10 * 60);
/// Open cards held at once; the oldest goes first.
pub(crate) const MAX_CARDS: usize = 16;

pub(crate) struct HeldCard {
    shown: Shown,
    at: Instant,
    /// D6: the "logged in" button, while a login's browser is open.
    /// Dropping it cancels the login with nothing saved.
    logged_in: Option<oneshot::Sender<()>>,
    /// An answer is running for this card.
    busy: bool,
}

#[derive(Default)]
pub struct HeldCards(Mutex<HashMap<String, HeldCard>>);

fn expired_text() -> String {
    format!(
        "this card is older than {} minutes or was already answered; open it again",
        CARD_TTL.as_secs() / 60
    )
}

impl HeldCards {
    fn held(&self) -> std::sync::MutexGuard<'_, HashMap<String, HeldCard>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn put(&self, shown: Shown) -> String {
        let id = uuid::Uuid::now_v7().to_string();
        let mut m = self.held();
        m.retain(|_, h| h.at.elapsed() < CARD_TTL);
        while m.len() >= MAX_CARDS {
            let Some(oldest) = m.iter().min_by_key(|(_, h)| h.at).map(|(k, _)| k.clone()) else {
                break;
            };
            m.remove(&oldest);
        }
        m.insert(
            id.clone(),
            HeldCard {
                shown,
                at: Instant::now(),
                logged_in: None,
                busy: false,
            },
        );
        id
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.held().len()
    }

    #[cfg(test)]
    pub(crate) fn age(&self, id: &str, by: Duration) {
        if let Some(h) = self.held().get_mut(id) {
            h.at = Instant::now().checked_sub(by).expect("clock");
        }
    }

    /// Start an answer on `id`: the card it was read as, and for a login
    /// the receiver its "logged in" button resolves.
    pub(crate) fn begin(
        &self,
        id: &str,
        login: bool,
    ) -> Result<(Shown, Option<oneshot::Receiver<()>>), String> {
        let mut m = self.held();
        let h = match m.get_mut(id) {
            Some(h) if h.at.elapsed() < CARD_TTL => h,
            Some(_) => {
                m.remove(id);
                return Err(expired_text());
            }
            None => return Err(expired_text()),
        };
        if h.busy {
            return Err("this card is already being answered".into());
        }
        h.busy = true;
        let rx = login.then(|| {
            let (tx, rx) = oneshot::channel();
            h.logged_in = Some(tx);
            rx
        });
        Ok((h.shown.clone(), rx))
    }

    /// The answer finished: drop the card when done, else keep it for
    /// another try with the clock it was read at.
    pub(crate) fn end(&self, id: &str, keep: bool) {
        let mut m = self.held();
        if keep {
            if let Some(h) = m.get_mut(id) {
                h.busy = false;
                h.logged_in = None;
            }
        } else {
            m.remove(id);
        }
    }

    /// The "logged in" button.
    pub(crate) fn logged_in(&self, id: &str) -> Result<(), String> {
        let tx = self
            .held()
            .get_mut(id)
            .and_then(|h| h.logged_in.take())
            .ok_or("this card has no login open")?;
        tx.send(()).map_err(|_| "the login already ended".into())
    }

    /// Close the card; an open login is cancelled with nothing saved.
    pub(crate) fn close(&self, id: &str) {
        self.held().remove(id);
    }
}

fn home() -> PathBuf {
    crate::mur_home_path()
}

fn canonical(name: &str) -> String {
    canonicalize_agent_name(&home(), name)
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("task failed: {e}"))?
}

/// The agent's open requests, one per site, compared around a one-shot
/// turn (no steps stream there) the way `ssh_admin::pending_stamp` is.
pub fn pending_stamp(mur_home: &std::path::Path, agent: &str) -> Vec<(String, i64)> {
    let mut v: Vec<(String, i64)> = front::pending(mur_home, agent)
        .into_iter()
        .map(|r| (r.site.origin(), r.requested_at.timestamp()))
        .collect();
    v.sort();
    v
}

#[tauri::command]
pub async fn agent_browser_pending(name: String) -> Result<Vec<PendingView>, String> {
    let name = canonical(&name);
    blocking(move || {
        Ok(front::pending(&home(), &name)
            .iter()
            .map(|r| view::pending_view(&name, r))
            .collect())
    })
    .await
}

#[tauri::command]
pub async fn agent_browser_card(
    cards: State<'_, HeldCards>,
    name: String,
    site: String,
) -> Result<CardView, String> {
    let name = canonical(&name);
    let site = Site::parse(&site).map_err(|e| e.to_string())?;
    let shown =
        blocking(move || front::show(&home(), &name, &site).map_err(|e| format!("{e:#}"))).await?;
    let v = view::card_view("", &shown);
    let id = cards.put(shown);
    Ok(CardView { id, ..v })
}

/// What the card's button sent. `Row.replace` must be the user's explicit
/// replace choice; `Login.reauth` is the card's `login_again_exists`.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PickArg {
    Row { name: String, replace: bool },
    Login { reauth: bool, replace: bool },
    Allow,
    Dismiss,
}

impl From<PickArg> for Pick {
    fn from(p: PickArg) -> Self {
        match p {
            PickArg::Row { name, replace } => Pick::Row { name, replace },
            PickArg::Login { reauth, replace } => Pick::Login {
                name: None,
                reauth,
                replace,
            },
            PickArg::Allow => Pick::Allow,
            PickArg::Dismiss => Pick::Dismiss,
        }
    }
}

/// Answer the card. For a login this returns only after the user pressed
/// "logged in" (`agent_browser_logged_in`) or closed the card. On `done`
/// the UI sends `hand_back` through the chat (D5).
#[tauri::command]
pub async fn agent_browser_answer(
    cards: State<'_, HeldCards>,
    id: String,
    pick: PickArg,
) -> Result<AnsweredView, String> {
    let pick: Pick = pick.into();
    let (shown, rx) = cards.begin(&id, matches!(pick, Pick::Login { .. }))?;
    let res = settle(home(), shown, pick, rx).await;
    cards.end(&id, !matches!(&res, Ok(v) if !v.kept));
    res
}

async fn settle(
    home: PathBuf,
    shown: Shown,
    pick: Pick,
    rx: Option<oneshot::Receiver<()>>,
) -> Result<AnsweredView, String> {
    // `settle` holds `&mut dyn` ops across awaits, so its future is not
    // `Send`: drive it on a blocking thread.
    let rt = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        rt.block_on(async move {
            let gate = async move {
                match rx {
                    Some(rx) => rx
                        .await
                        .map_err(|_| anyhow::anyhow!("the login was cancelled")),
                    None => Ok(()),
                }
            };
            let mut say = |l: String| tracing::info!(target: "browser_card", "{l}");
            let settled = front::settle(&home, &shown, pick, gate, &mut say)
                .await
                .map_err(|e| format!("{e:#}"))?;
            let status = settled
                .saved_as
                .as_deref()
                .and_then(|p| front::status(&home, p).ok());
            Ok(view::answered_view(settled, status))
        })
    })
    .await
    .map_err(|e| format!("task failed: {e}"))?
}

#[tauri::command]
pub fn agent_browser_logged_in(cards: State<'_, HeldCards>, id: String) -> Result<(), String> {
    cards.logged_in(&id)
}

#[tauri::command]
pub fn agent_browser_close(cards: State<'_, HeldCards>, id: String) {
    cards.close(&id);
}
