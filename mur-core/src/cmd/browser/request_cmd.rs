//! `mur browser requests` / `mur browser answer`: the request card on the
//! command line (#2360 slice 3). murmur and Hub render the same
//! [`request_card`](super::request_card) data as buttons (slice 4).

use std::io::Write;

use anyhow::{Result, bail};
use mur_browser_request::{PendingRequest, Site};

use super::live_effect::Effect;
#[cfg(test)]
use super::request_card::Choice;
use super::request_card::front::{self, Pick};
use super::request_card::{self as card, Answered, Card, Offer, Reach};

pub fn requests(agent: Option<&str>) -> Result<()> {
    let home = super::mur_home()?;
    let all = crate::a2a_dial::agent_dir_names(&home);
    let mut agents = match agent {
        Some(a) => vec![one_agent(&all, a)?],
        None => all,
    };
    agents.sort();
    let entries = card::live_entries(&home);
    let mut out = std::io::stdout();
    let mut any = false;
    for a in agents {
        for r in card::open_requests(&home, &a) {
            any = true;
            // Best-effort (council N9): one unreadable agent does not hide
            // every other agent's requests.
            match card::card_for_in(&home, &entries, &a, &r) {
                Ok((c, _)) => {
                    for line in render(&a, &r, &c) {
                        writeln!(out, "{line}")?;
                    }
                }
                Err(e) => eprintln!("{a}: request for {} not shown: {e:#}", r.site.origin()),
            }
        }
    }
    if !any {
        writeln!(out, "no browser requests waiting.")?;
    }
    Ok(())
}

/// `--agent`, resolved like every agent name (case-insensitive). A name
/// that matches no agent is an error, not "no requests waiting".
fn one_agent(all: &[String], typed: &str) -> Result<String> {
    use crate::a2a_dial::{NameMatch, match_agent_name};
    match match_agent_name(all, typed) {
        NameMatch::Found(name) => Ok(name),
        NameMatch::Missing => bail!("no agent named {typed:?} (see `mur agent list`)"),
        NameMatch::Ambiguous(names) => {
            bail!("{typed:?} matches several agents: {}", names.join(", "))
        }
    }
}

fn render(agent: &str, r: &PendingRequest, c: &Card) -> Vec<String> {
    let site = r.site.origin();
    let cmd = format!("mur browser answer {agent} {site}");
    let mut out = vec![format!(
        "{agent} asks for {site}{}: \"{}\"",
        if r.login { " (login needed)" } else { "" },
        r.reason
    )];
    let port = match c {
        Card::NoLiveEntry => {
            out.push(format!(
                "  {agent} has no live browser entry. Create one inside murmur, in the \
                 {agent} conversation: /browser live {}",
                r.site.host()
            ));
            None
        }
        Card::Login {
            rows,
            login_again,
            login_offer,
            port,
        } => {
            if rows.is_empty() {
                out.push("  no saved login matches this site.".into());
            }
            for row in rows {
                let shared = if row.shared_with.is_empty() {
                    String::new()
                } else {
                    format!(" (shared with {})", row.shared_with.join(", "))
                };
                let pick = format!("{cmd} --profile {}", row.name);
                let what = match &row.offer {
                    Offer::AlreadyBound => {
                        format!("already set up → {pick}   (clears the request)")
                    }
                    Offer::Offered => format!("→ {pick}"),
                    Offer::AddsHosts { missing } => {
                        format!("also allows {} → {pick}", missing.join(", "))
                    }
                    Offer::ReplaceOnly {
                        with,
                        domains,
                        drops,
                        adds,
                    } => {
                        let adds = if adds.is_empty() {
                            String::new()
                        } else {
                            format!(", allows {}", adds.join(", "))
                        };
                        format!(
                            "shares {} with bound {with:?}; only as a replacement \
                             (drops {}{adds}) → {pick} --replace",
                            domains.join(", "),
                            drops.join(", ")
                        )
                    }
                    Offer::Disabled { why } => format!("not available: {why}"),
                };
                out.push(format!("  {}{shared}: {what}", row.name));
            }
            // #2461: a login that only binds as a replacement needs
            // `--replace` too, or the answer is refused before it opens.
            let (replace, drops) = match login_offer {
                Offer::ReplaceOnly { drops, .. } => (
                    " --replace",
                    format!("; replaces the binding {}", drops.join(", ")),
                ),
                _ => ("", String::new()),
            };
            let again = match login_offer {
                Offer::Disabled { why } => format!("  log in again: not available: {why}"),
                _ if login_again.exists => format!(
                    "  log in again → {cmd} --login --reauth{replace}   (replaces the saved \
                     login {:?}{drops}; --name to save under another name)",
                    login_again.name
                ),
                _ => format!(
                    "  log in again → {cmd} --login{replace}   (saves as {:?}{drops}; \
                     --name to change)",
                    login_again.name
                ),
            };
            out.push(again);
            *port
        }
        Card::Reach { reach, port } => {
            out.push(match reach {
                Reach::Allowed => format!("  reachable already → {cmd} --allow   (clears the request)"),
                Reach::NeedsHost { missing } => format!("  allows {} → {cmd} --allow", missing.join(", ")),
                Reach::Unscoped { mode } => format!(
                    "  the browser entry is not host-scoped (network mode {mode}); the agent's \
                     own network policy decides → {cmd} --allow   (writes nothing, clears the request)"
                ),
                Reach::Disabled { why } => format!("  not available: {why}"),
            });
            *port
        }
    };
    if let Some(p) = port {
        out.push(format!(
            "  port {p} is not open: add --open-port (opens port {p} to ANY host; restart needed)"
        ));
    }
    out.push(format!("  dismiss → {cmd} --dismiss"));
    out
}

pub struct AnswerArgs<'a> {
    pub agent: &'a str,
    pub site: &'a str,
    pub profile: Option<&'a str>,
    pub login: bool,
    pub name: Option<&'a str>,
    pub reauth: bool,
    pub allow: bool,
    pub replace: bool,
    pub open_port: bool,
    pub dismiss: bool,
}

/// The flags as a card pick, for every answer but `--login` (whose row
/// does not exist until the plan is made). Every `Choice` comes from a row
/// or a reach the card printed (D11); [`front::choice_for`] decides it.
#[cfg(test)]
fn choose(c: &Card, a: &AnswerArgs<'_>) -> Result<Choice, String> {
    front::choice_for(c, &pick_of(c, a)?)
}

fn pick_of(c: &Card, a: &AnswerArgs<'_>) -> Result<Pick, String> {
    Ok(match (a.dismiss, a.profile, a.allow) {
        (true, ..) => Pick::Dismiss,
        (_, Some(p), _) => Pick::Row {
            name: p.into(),
            replace: a.replace,
        },
        (_, None, true) => Pick::Allow,
        (_, None, false) => {
            return Err(match c {
                Card::NoLiveEntry => "the agent has no live browser entry".into(),
                Card::Login { .. } => {
                    "this request needs a login: pick --profile <row>, --login, or --dismiss".into()
                }
                Card::Reach { .. } => "answer with --allow or --dismiss".into(),
            });
        }
    })
}

pub async fn answer(a: AnswerArgs<'_>) -> Result<()> {
    let home = super::mur_home()?;
    let agent = crate::a2a_dial::canonicalize_agent_name(&home, a.agent);
    let want = Site::parse(a.site).map_err(|e| anyhow::anyhow!("{e}"))?;
    let shown = front::show(&home, &agent, &want)?;
    if a.open_port && shown.port().is_none() {
        println!("no port to open for {}.", want.origin());
    }
    let pick = if a.login {
        Pick::Login {
            name: a.name.map(str::to_owned),
            reauth: a.reauth,
            replace: a.replace,
        }
    } else {
        let nothing = |why| anyhow::anyhow!("✗ {agent}: {why} — nothing written");
        let pick = pick_of(&shown.card, &a).map_err(nothing)?;
        front::choice_for(&shown.card, &pick).map_err(nothing)?;
        pick
    };
    let mut say = |l: String| println!("{l}");
    let run_login = async |plan: &card::LoginPlan, say: &mut (dyn FnMut(String) + Send)| {
        let engine = super::auth::card_browser(&home)?;
        say(format!(
            "opening {} for the login.",
            engine.playwright_name()
        ));
        card::login(
            &home,
            plan,
            engine.playwright_name(),
            mur_browser::proxy::enter_on_stdin(),
        )
        .await?;
        Ok(())
    };
    // `--open-port` is the one path that grants a port (D1, #2456).
    let front::Settled { answered, saved_as } = front::settle_with(
        &home,
        &shown,
        pick,
        a.open_port,
        run_login,
        &mut say,
        &mut crate::cmd::agent::cli::browser_live_cmd::ProfileOps(&agent),
        &mut card::AgentPorts(&agent),
    )
    .await?;
    let port = shown.port();
    // A refusal after a login must not say "nothing written" (council B4).
    let kept = match &saved_as {
        Some(p) => format!("the login is saved as {p:?}; the request stays open"),
        None => "nothing written; the request stays open".into(),
    };
    match answered {
        Answered::Done {
            effect,
            notes,
            hand_back,
        } => {
            print_applied(effect, &notes);
            if let Some(p) = &saved_as {
                println!("saved, not yet tried: {p:?}");
            }
            if let Some(h) = hand_back {
                println!("tell {agent}: {h}");
            }
            Ok(())
        }
        Answered::PortFailed {
            effect,
            notes,
            error,
        } => {
            print_applied(effect, &notes);
            bail!(
                "✗ {agent}: the binding is saved, but port {} was not opened: {error}. \
                 The request stays open; run `mur agent perm allow-port {agent} {}`, then answer it again.",
                port.unwrap_or_default(),
                port.unwrap_or_default()
            )
        }
        Answered::Refused(why) => bail!("✗ {agent}: {why} — {kept}"),
        Answered::Stale => bail!(
            "✗ {agent}'s browser entry changed since the card was read — {kept}. \
             Run `mur browser requests` again."
        ),
    }
}

fn print_applied(effect: Option<Effect>, notes: &[String]) {
    for n in notes {
        println!("{n}");
    }
    if let Some(e) = effect {
        println!("{}", e.applied_line());
    }
}

#[cfg(test)]
#[path = "request_cmd_tests.rs"]
mod tests;
