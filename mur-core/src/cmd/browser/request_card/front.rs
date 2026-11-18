//! The one door murmur, Hub and `mur browser answer` use (slice 4, D2):
//! read a request's card, then settle it with the row the user picked.
//! The snapshot the card was built from stays private to [`Shown`], so an
//! answer only ever lands on the card the user saw (S3:D11).
//!
//! No public path here grants a port (D1, #2456): `--open-port` opens the
//! port to any host, so it stays a typed CLI flag. [`port_line`] gives the
//! UIs the line to show instead.

use std::future::Future;
use std::path::Path;

use anyhow::{Result, bail};
use mur_browser::paths;
use mur_browser_request::Site;

use super::super::live_entry::{LIVE_ENTRY, LiveOps, norm};
use super::apply::{Answer, Choice, PortOps};
use super::login::LoginPlan;
use super::ops::Snapshot;

pub use super::super::live_effect::Effect;
pub use super::apply::Answered;
pub use super::model::{Card, LoginAgain, Offer, ProfileRow, Reach};
pub use mur_browser_request::PendingRequest;

/// A card as the user sees it, plus the entries it was built from.
#[derive(Debug, Clone)]
pub struct Shown {
    pub agent: String,
    pub request: PendingRequest,
    pub card: Card,
    snapshot: Snapshot,
}

impl Shown {
    /// The card's port step, if the site's port is not open for the agent.
    pub fn port(&self) -> Option<u16> {
        match &self.card {
            Card::Login { port, .. } | Card::Reach { port, .. } => *port,
            Card::NoLiveEntry => None,
        }
    }

    /// A card over no entries, for UI tests (murmur, Hub) that never
    /// settle: any answer against it is `Stale`.
    #[doc(hidden)]
    pub fn for_test(agent: &str, request: PendingRequest, card: Card) -> Self {
        Self {
            agent: agent.into(),
            request,
            card,
            snapshot: Vec::new(),
        }
    }
}

/// What the user picked on the card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// A profile row; `replace` is the explicit choice a `ReplaceOnly` row needs.
    Row {
        name: String,
        replace: bool,
    },
    /// "Log in again". A button passes `name: None` and
    /// `reauth: login_again.exists`; the CLI passes its flags.
    Login {
        name: Option<String>,
        reauth: bool,
        replace: bool,
    },
    Allow,
    Dismiss,
}

/// The answer, and the login it saved first (if any): a refusal after a
/// login must not say "nothing written" (council B4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settled {
    pub answered: Answered,
    pub saved_as: Option<String>,
}

/// What a saved login's card label says (D4). Neither is a MUR check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Saved, and no agent has used it and written state back yet.
    SavedNotTried,
    /// An agent used it and wrote its state back at this time.
    AgentReported(chrono::DateTime<chrono::Utc>),
}

/// `agent`'s open browser requests.
pub fn pending(home: &Path, agent: &str) -> Vec<PendingRequest> {
    super::open_requests(home, agent)
}

/// The card for `agent`'s open request for `site`.
pub fn show(home: &Path, agent: &str, site: &Site) -> Result<Shown> {
    let Some(request) = pending(home, agent).into_iter().find(|r| r.site == *site) else {
        bail!(
            "{agent} has no browser request for {} (see `mur browser requests`)",
            site.origin()
        );
    };
    let (card, snapshot) = super::card_for(home, agent, &request)?;
    Ok(Shown {
        agent: agent.to_owned(),
        request,
        card,
        snapshot,
    })
}

/// D1: the line a UI shows for the port step — the CLI command, and that
/// it opens the port to ANY host. `None` when there is no port step.
pub fn port_line(s: &Shown) -> Option<String> {
    s.port().map(|p| {
        format!(
            "port {p} is not open. This opens port {p} to ANY host, so it is a CLI step: \
             mur browser answer {} {} --open-port (restart needed)",
            s.agent,
            s.request.site.origin()
        )
    })
}

/// Settle `s` with `pick`. `logged_in` is the card's "logged in" action; it
/// is awaited only for [`Pick::Login`], and resolving to an error (the card
/// closed) cancels the login with nothing saved. `say` gets the lines worth
/// showing while it runs (a replace warning, the browser being opened).
pub async fn settle(
    home: &Path,
    s: &Shown,
    pick: Pick,
    logged_in: impl Future<Output = Result<()>>,
    say: &mut (dyn FnMut(String) + Send),
) -> Result<Settled> {
    let run_login = async |plan: &LoginPlan, say: &mut (dyn FnMut(String) + Send)| {
        let engine = super::super::auth::card_browser(home)?;
        say(format!(
            "opening {} for the login.",
            engine.playwright_name()
        ));
        super::login(home, plan, engine.playwright_name(), logged_in).await?;
        Ok(())
    };
    let agent = s.agent.clone();
    settle_with(
        home,
        s,
        pick,
        false,
        run_login,
        say,
        &mut crate::cmd::agent::cli::browser_live_cmd::ProfileOps(&agent),
        &mut super::AgentPorts(&agent),
    )
    .await
}

/// [`settle`] with its side effects injected. `open_port` is `true` only
/// from the CLI's typed `--open-port` (D1).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn settle_with(
    home: &Path,
    s: &Shown,
    pick: Pick,
    open_port: bool,
    run_login: impl AsyncFnOnce(&LoginPlan, &mut (dyn FnMut(String) + Send)) -> Result<()>,
    say: &mut (dyn FnMut(String) + Send),
    ops: &mut dyn LiveOps,
    ports: &mut dyn PortOps,
) -> Result<Settled> {
    let agent = &s.agent;
    let mut saved_as = None;
    let choice = if let Pick::Login {
        name,
        reauth,
        replace,
    } = &pick
    {
        let (plan, choice) = login_choice(home, s, name.as_deref(), *reauth, *replace)?;
        if plan.replace {
            say(format!("this replaces the saved login {:?}.", plan.profile));
            let others = super::shared_with(home, agent, &plan.profile);
            if !others.is_empty() {
                say(format!(
                    "  {} also use it and get the new login on their next restart.",
                    others.join(", ")
                ));
            }
        }
        run_login(&plan, say).await?;
        saved_as = Some(plan.profile);
        choice
    } else {
        choice_for(&s.card, &pick)
            .map_err(|why| anyhow::anyhow!("✗ {agent}: {why} — nothing written"))?
    };
    let bound = super::super::bind::bound_domains(home);
    let answer = Answer {
        agent,
        agent_home: &home.join("agents").join(agent),
        request: &s.request,
        port: s.port(),
        snapshot: &s.snapshot,
        login_saved: saved_as.as_deref(),
    };
    let answered = super::answer(&answer, &choice, open_port, &bound, ops, ports)?;
    Ok(Settled { answered, saved_as })
}

/// The row a login would bind, decided before anyone signs in (council B4).
fn login_choice(
    home: &Path,
    s: &Shown,
    name: Option<&str>,
    reauth: bool,
    replace: bool,
) -> Result<(LoginPlan, Choice)> {
    let agent = &s.agent;
    if matches!(s.card, Card::Reach { .. }) {
        bail!("✗ {agent}: this request needs no login: answer with --allow or --dismiss");
    }
    let plan = super::login_plan(home, &s.request.site, name, reauth)?;
    let Some(entry) = s.snapshot.iter().find(|e| e.name == LIVE_ENTRY) else {
        bail!("✗ {agent}: no live browser entry — nothing written");
    };
    let row = super::Profile {
        name: plan.profile.clone(),
        domains: plan.allow_domains.clone(),
    };
    let bound = super::super::bind::bound_domains(home);
    let offer = super::offer_for(entry, &row, &norm(s.request.site.host()), &bound);
    let choice = super::use_offer(&plan.profile, &plan.allow_domains, &offer, replace)
        .map_err(|why| anyhow::anyhow!("✗ {agent}: {why} — nothing written, no browser opened"))?;
    Ok((plan, choice))
}

/// Every `Choice` comes from a row or a reach the card showed (D11).
/// `Pick::Login` has no row until its plan is made; see [`login_choice`].
pub(crate) fn choice_for(c: &Card, pick: &Pick) -> Result<Choice, String> {
    match (c, pick) {
        (_, Pick::Dismiss) => Ok(Choice::Dismiss),
        (_, Pick::Login { .. }) => Err("a login is settled through its plan".into()),
        (Card::NoLiveEntry, _) => Err("the agent has no live browser entry".into()),
        (Card::Login { rows, .. }, Pick::Row { name, replace }) => {
            let Some(row) = rows.iter().find(|r| r.name == *name) else {
                let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
                return Err(format!(
                    "{name:?} is not on this card (rows: {}); see `mur browser requests`",
                    if names.is_empty() {
                        "none".into()
                    } else {
                        names.join(", ")
                    }
                ));
            };
            super::use_offer(&row.name, &row.domains, &row.offer, *replace)
        }
        (Card::Login { .. }, Pick::Allow) => {
            Err("this request needs a login: pick --profile <row>, --login, or --dismiss".into())
        }
        (Card::Reach { .. }, Pick::Row { .. }) => {
            Err("this request needs no login: answer with --allow or --dismiss".into())
        }
        (Card::Reach { reach, .. }, Pick::Allow) => match reach {
            Reach::Allowed | Reach::Unscoped { .. } => Ok(Choice::Allow { approved: vec![] }),
            Reach::NeedsHost { missing } => Ok(Choice::Allow {
                approved: missing.clone(),
            }),
            Reach::Disabled { why } => Err(format!("not available: {why}")),
        },
    }
}

/// The label a saved login's row carries (D4).
pub fn status(home: &Path, profile: &str) -> Result<Status> {
    paths::validate_name(profile)?;
    let meta = super::super::read_profile_meta(&paths::profile_meta(home, profile))?;
    Ok(match meta.last_refresh {
        None => Status::SavedNotTried,
        Some(t) => Status::AgentReported(t),
    })
}

#[cfg(test)]
#[path = "front_tests.rs"]
mod tests;
