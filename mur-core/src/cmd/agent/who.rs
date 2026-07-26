//! `mur agent who` — the dispatch index, for humans.
//!
//! The counterpart to the runtime's spawn-denial hint. The hint tells an agent
//! only the routes it may already use; this tells the *user* the whole picture,
//! including the capable-but-unauthorized fleets and the exact command that
//! would authorize them. Deny-by-default only works if the grant path is
//! discoverable — otherwise people route around it by handing the front-door
//! agent the dangerous binary, which is the outcome the sandbox exists to
//! prevent.

use anyhow::Result;
use mur_common::agent_facts::{AgentFacts, Blocker, ExecFacts, scan_agents, who_can_exec};

use super::resolve_mur_home;

fn exec_summary(f: &AgentFacts) -> String {
    match &f.exec {
        ExecFacts::Unrestricted => "ANY".to_string(),
        ExecFacts::Nothing => "—".to_string(),
        ExecFacts::Allowlist(l) => l
            .iter()
            .map(|b| {
                std::path::Path::new(b)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| b.clone())
            })
            .collect::<Vec<_>>()
            .join(", "),
    }
}

fn print_agent(f: &AgentFacts) {
    let state = match (f.running, f.drift) {
        (true, true) => " [running, PROFILE EDITED SINCE START]",
        (true, false) => " [running]",
        (false, _) => "",
    };
    println!("  {}{}", f.name, state);
    if !f.role.is_empty() {
        println!("      role   {}", f.role);
    }
    println!(
        "      exec   {}\n      writes {}\n      net    {:?}",
        exec_summary(f),
        if f.writes.is_empty() {
            "—".to_string()
        } else {
            f.writes
                .iter()
                .map(|w| w.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        },
        f.net
    );
    println!(
        "      effort {}",
        match f.effort {
            Some(e) => e.as_str(),
            None => "— (unset: the API default is high)",
        }
    );
    if !f.skills.is_empty() {
        println!("      skills {}", f.skills.join(", "));
    }
}

pub fn cmd_who(can: Option<String>, skill: Option<String>, as_agent: Option<String>) -> Result<()> {
    let home = resolve_mur_home()?;
    let requester = as_agent.unwrap_or_else(|| mur_common::fleet::CONCIERGE_AGENT.to_string());

    let agents: Vec<AgentFacts> = scan_agents(&home)
        .into_iter()
        .filter(|a| can.as_deref().is_none_or(|b| a.can_exec(b)))
        .filter(|a| {
            skill.as_deref().is_none_or(|s| {
                a.skills
                    .iter()
                    .any(|k| k.to_lowercase().contains(&s.to_lowercase()))
            })
        })
        .collect();

    match (&can, &skill) {
        (Some(b), _) => println!("Agents that explicitly hold `{b}`:"),
        (None, Some(s)) => println!("Agents with a skill matching '{s}':"),
        (None, None) => println!("All agents:"),
    }
    if agents.is_empty() {
        println!("  (none)");
    }
    for a in &agents {
        print_agent(a);
    }

    // Dispatch routes only make sense for a concrete binary.
    let Some(bin) = can else {
        return Ok(());
    };
    let routes = who_can_exec(&home, &requester, &bin, None);
    println!("\nDispatch routes for '{requester}' (fleet_run), best first:");
    if routes.ready.is_empty() {
        println!("  ready:   (none)");
    } else {
        println!("  ready:");
        for f in &routes.ready {
            let via: Vec<&str> = f
                .members_with(&bin)
                .iter()
                .map(|m| m.name.as_str())
                .collect();
            println!(
                "    {:<20} via {:<24} budget ${:.2}",
                f.name,
                via.join(", "),
                f.budget_usd
            );
        }
    }
    if !routes.blocked.is_empty() {
        println!("  blocked (capable, but not usable right now):");
        for f in &routes.blocked {
            let via: Vec<&str> = f
                .members_with(&bin)
                .iter()
                .map(|m| m.name.as_str())
                .collect();
            let Some(b) = f.blocker() else { continue };
            println!(
                "    {:<20} via {:<24} — {}",
                f.name,
                via.join(", "),
                b.as_str()
            );
            match b {
                Blocker::NotAuthorized => println!(
                    "      authorize: add \"{}\" to fleet_run.fleets in {}",
                    f.name,
                    home.join("config.yaml").display()
                ),
                Blocker::NoBudget => println!(
                    "      authorize: set loop.budget_usd in {}",
                    home.join("fleets")
                        .join(&f.name)
                        .join("fleet.yaml")
                        .display()
                ),
            }
        }
    }
    Ok(())
}
