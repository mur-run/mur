//! `mur browser requests` / `mur browser answer`: the request card on the
//! command line (#2360 slice 3). murmur and Hub render the same
//! [`request_card`](super::request_card) data as buttons (slice 4).

use std::io::Write;

use anyhow::{Result, bail};
use mur_browser_request::{PendingRequest, Site};

use super::live_effect::Effect;
use super::live_entry::{LIVE_ENTRY, norm};
use super::request_card::{
    self as card, Answer, Answered, Card, Choice, Offer, Profile, Reach, use_offer,
};

pub fn requests(agent: Option<&str>) -> Result<()> {
    let home = super::mur_home()?;
    let mut agents = match agent {
        Some(a) => vec![crate::a2a_dial::canonicalize_agent_name(&home, a)],
        None => crate::a2a_dial::agent_dir_names(&home),
    };
    agents.sort();
    let mut out = std::io::stdout();
    let mut any = false;
    for a in agents {
        for r in card::open_requests(&home, &a) {
            any = true;
            // Best-effort (council N9): one unreadable agent does not hide
            // every other agent's requests.
            match card::card_for(&home, &a, &r) {
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
            let again = if login_again.exists {
                format!(
                    "  log in again → {cmd} --login --reauth   (replaces the saved login {:?}; \
                     --name to save under another name)",
                    login_again.name
                )
            } else {
                format!(
                    "  log in again → {cmd} --login   (saves as {:?}; --name to change)",
                    login_again.name
                )
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

/// The flags against the card, for every answer but `--login` (whose row
/// does not exist until the plan is made). Every `Choice` comes from a row
/// or a reach the card printed (D11).
fn choose(c: &Card, a: &AnswerArgs<'_>) -> Result<Choice, String> {
    if a.dismiss {
        return Ok(Choice::Dismiss);
    }
    match (c, a.profile, a.allow) {
        (Card::NoLiveEntry, ..) => Err("the agent has no live browser entry".into()),
        (Card::Login { rows, .. }, Some(p), _) => {
            let Some(row) = rows.iter().find(|r| r.name == p) else {
                let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
                return Err(format!(
                    "{p:?} is not on this card (rows: {}); see `mur browser requests`",
                    if names.is_empty() {
                        "none".into()
                    } else {
                        names.join(", ")
                    }
                ));
            };
            use_offer(&row.name, &row.domains, &row.offer, a.replace)
        }
        (Card::Login { .. }, None, _) => {
            Err("this request needs a login: pick --profile <row>, --login, or --dismiss".into())
        }
        (Card::Reach { .. }, Some(_), _) => {
            Err("this request needs no login: answer with --allow or --dismiss".into())
        }
        (Card::Reach { reach, .. }, None, true) => match reach {
            Reach::Allowed | Reach::Unscoped { .. } => Ok(Choice::Allow { approved: vec![] }),
            Reach::NeedsHost { missing } => Ok(Choice::Allow {
                approved: missing.clone(),
            }),
            Reach::Disabled { why } => Err(format!("not available: {why}")),
        },
        (Card::Reach { .. }, None, false) => Err("answer with --allow or --dismiss".into()),
    }
}

pub async fn answer(a: AnswerArgs<'_>) -> Result<()> {
    let home = super::mur_home()?;
    let agent = crate::a2a_dial::canonicalize_agent_name(&home, a.agent);
    let want = Site::parse(a.site).map_err(|e| anyhow::anyhow!("{e}"))?;
    let Some(req) = card::open_requests(&home, &agent)
        .into_iter()
        .find(|r| r.site == want)
    else {
        bail!(
            "{agent} has no browser request for {} (see `mur browser requests`)",
            want.origin()
        );
    };
    let (shown, snapshot) = card::card_for(&home, &agent, &req)?;
    let port = match &shown {
        Card::Login { port, .. } | Card::Reach { port, .. } => *port,
        Card::NoLiveEntry => None,
    };
    if a.open_port && port.is_none() {
        println!("no port to open for {}.", req.site.origin());
    }
    let bound = super::bind::bound_domains(&home);
    let mut saved_as = None;
    let choice = if a.login {
        if matches!(shown, Card::Reach { .. }) {
            bail!("✗ {agent}: this request needs no login: answer with --allow or --dismiss");
        }
        let plan = card::login_plan(&home, &req.site, a.name, a.reauth)?;
        // Council B4: decide the row before anyone signs in.
        let entry = snapshot.iter().find(|e| e.name == LIVE_ENTRY);
        let Some(entry) = entry else {
            bail!("✗ {agent}: no live browser entry — nothing written");
        };
        let row = Profile {
            name: plan.profile.clone(),
            domains: plan.allow_domains.clone(),
        };
        let offer = card::offer_for(entry, &row, &norm(req.site.host()), &bound);
        let choice =
            use_offer(&plan.profile, &plan.allow_domains, &offer, a.replace).map_err(|why| {
                anyhow::anyhow!("✗ {agent}: {why} — nothing written, no browser opened")
            })?;
        if plan.replace {
            let others = card::shared_with(&home, &agent, &plan.profile);
            println!("this replaces the saved login {:?}.", plan.profile);
            if !others.is_empty() {
                println!(
                    "  {} also use it and get the new login on their next restart.",
                    others.join(", ")
                );
            }
        }
        let engine = super::auth::card_browser(&home)?;
        card::login(
            &home,
            &plan,
            engine.playwright_name(),
            mur_browser::proxy::enter_on_stdin(),
        )
        .await?;
        saved_as = Some(plan.profile);
        choice
    } else {
        choose(&shown, &a).map_err(|why| anyhow::anyhow!("✗ {agent}: {why} — nothing written"))?
    };
    let answer = Answer {
        agent: &agent,
        agent_home: &home.join("agents").join(&agent),
        request: &req,
        port,
        snapshot: &snapshot,
        login_saved: saved_as.as_deref(),
    };
    let mut ops = crate::cmd::agent::cli::browser_live_cmd::ProfileOps(&agent);
    let got = card::answer(
        &answer,
        &choice,
        a.open_port,
        &bound,
        &mut ops,
        &mut card::AgentPorts(&agent),
    )?;
    // A refusal after a login must not say "nothing written" (council B4).
    let kept = match &saved_as {
        Some(p) => format!("the login is saved as {p:?}; the request stays open"),
        None => "nothing written; the request stays open".into(),
    };
    match got {
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
