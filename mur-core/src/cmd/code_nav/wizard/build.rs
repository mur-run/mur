//! Turn the selection into targets: name each (agent, project) entry, pick
//! each project's languages once, and plan every target with the same
//! `target_plan` / `consent::diff` single-agent setup uses. Writes nothing.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;

use anyhow::Result;

use super::super::consent;
use super::super::plan::{Flags, Lang};
use super::super::setup::{detect, target_plan};
use super::names::{self, Naming};
use super::prompt::Prompt;
use super::select;
use super::{AgentCtx, Decided, Hold, Project, Spec};

/// The answer to "which project does unrecorded entry X serve?".
enum Unknown {
    Serves(usize),
    Neither,
    Skip,
}

pub(super) fn build(
    mur_home: &Path,
    agents: Vec<AgentCtx>,
    projects: Vec<Project>,
    per_agent: Vec<BTreeSet<usize>>,
    ask: bool,
    prompt: &mut dyn Prompt,
    out: &mut dyn Write,
) -> Result<Decided> {
    let used: BTreeSet<usize> = per_agent.iter().flatten().copied().collect();
    let mut langs: Vec<BTreeSet<Lang>> = vec![BTreeSet::new(); projects.len()];
    for i in &used {
        langs[*i] = select::languages(&projects[*i], ask, prompt)?;
    }
    let detected = detect();
    let mut d = Decided {
        agents: Vec::new(),
        projects,
        specs: Vec::new(),
        kept: Vec::new(),
        duplicates: Vec::new(),
    };
    for (ai, (agent, picked)) in agents.into_iter().zip(per_agent).enumerate() {
        let unknown = resolve_unknown(&agent, &picked, &d.projects, ask, prompt)?;
        for (name, p) in &agent.existing.known {
            if let Some(pi) = d.projects.iter().position(|x| &x.path == p)
                && !picked.contains(&pi)
            {
                d.kept.push((ai, name.clone(), p.clone()));
            }
        }
        let mut assigned: Vec<String> = Vec::new();
        let start = d.specs.len();
        for &pi in &picked {
            let project = &d.projects[pi];
            let mut hold = unknown.hold.clone();
            let naming = match unknown.serves.iter().find(|(_, p)| *p == pi) {
                Some((name, _)) => Naming::Reuse {
                    entry: name.clone(),
                    duplicates: Vec::new(),
                },
                None => name_target(&agent, project, &assigned, ask, prompt)?,
            };
            if let Naming::Reuse { entry, duplicates } = &naming
                && !duplicates.is_empty()
            {
                d.duplicates.push((ai, entry.clone(), duplicates.clone()));
            }
            assigned.push(naming.entry().to_owned());
            let mut lsp: BTreeSet<Lang> = langs[pi].clone();
            if lsp.is_empty()
                && let Some(prior) = agent.prior.languages_of(naming.entry())
            {
                lsp = prior.iter().filter_map(|l| Lang::from_flag(l)).collect();
            }
            if lsp.is_empty() && hold.is_none() {
                hold = Some(Hold::Skipped(no_language_reason(project)));
            }
            // ast-grep stays in every target's plan: installing and wiring it
            // again is a no-op, and a target never depends on another's.
            let flags = Flags {
                with_serena: true,
                no_ast_grep: false,
                lsp: lsp.iter().map(|l| l.flag().to_owned()).collect(),
                exact_lsp: false,
            };
            d.specs.push(spec(
                mur_home,
                &agent,
                ai,
                Some((pi, &project.path)),
                Some(naming),
                flags,
                hold,
                &detected,
            ));
        }
        // No serena target left to carry ast-grep: give the agent an
        // ast-grep-only target, as setup without --with-serena.
        if d.specs[start..].iter().all(|s| s.hold.is_some()) {
            d.specs.push(spec(
                mur_home,
                &agent,
                ai,
                None,
                None,
                Flags::default(),
                None,
                &detected,
            ));
        }
        d.agents.push(agent);
    }
    writeln!(out)?;
    Ok(d)
}

/// Why a project gets no serena entry: nothing detected, or only languages
/// that need an opt-in, named so the user sees what was found.
pub(super) fn no_language_reason(p: &Project) -> String {
    let opt_in: Vec<&str> = p
        .detected
        .iter()
        .filter(|l| !l.on_by_default())
        .map(|l| l.flag())
        .collect();
    if opt_in.is_empty() {
        return "no language server selected for this project".into();
    }
    format!(
        "detected {} — opt-in only; tick them in the language picker, or run the printed \
         single-agent command with --lsp",
        opt_in.join(", ")
    )
}

#[allow(clippy::too_many_arguments)]
fn spec(
    mur_home: &Path,
    agent: &AgentCtx,
    ai: usize,
    project: Option<(usize, &Path)>,
    naming: Option<Naming>,
    flags: Flags,
    mut hold: Option<Hold>,
    detected: &super::super::plan::Detected,
) -> Spec {
    let entry = naming
        .as_ref()
        .map_or(
            mur_agent_runtime::mcp::serena::DEFAULT_ENTRY_NAME,
            Naming::entry,
        )
        .to_owned();
    let path = project.map(|(_, p)| p);
    let plan = match target_plan(mur_home, &agent.name, &flags, path, detected) {
        Ok(p) => p,
        Err(e) => {
            hold.get_or_insert(Hold::Blocked(format!("{e:#}")));
            Default::default()
        }
    };
    let diff = consent::diff(&plan, &entry, path, &agent.prior);
    Spec {
        agent: ai,
        project: project.map(|(i, _)| i),
        naming,
        flags,
        plan,
        diff,
        hold,
    }
}

/// Name the entry `project` gets on `agent`. A suffixed name can be edited
/// interactively; `--yes` takes the printed one.
fn name_target(
    agent: &AgentCtx,
    project: &Project,
    assigned: &[String],
    ask: bool,
    prompt: &mut dyn Prompt,
) -> Result<Naming> {
    let naming = names::name_for(&agent.existing, &project.path, &project.base, assigned);
    let Naming::New {
        entry,
        wanted: Some(wanted),
    } = &naming
    else {
        return Ok(naming);
    };
    if !ask {
        return Ok(naming);
    }
    let mut proposal = entry.clone();
    loop {
        let typed = prompt.input(
            &format!(
                "{}: `{wanted}` is taken; entry name for {}",
                agent.name,
                project.path.display()
            ),
            &proposal,
        )?;
        let typed = typed.trim().to_owned();
        if names::usable(&agent.existing, &typed, assigned) {
            return Ok(Naming::New {
                entry: typed.clone(),
                wanted: (typed != *wanted).then(|| wanted.clone()),
            });
        }
        proposal = entry.clone();
    }
}

struct UnknownAnswer {
    /// Unrecorded entries the user tied to a picked project.
    serves: Vec<(String, usize)>,
    /// Applies to every serena target of the agent.
    hold: Option<Hold>,
}

/// Decision 4: a `kind: serena` entry with no manifest record serves an
/// unknown project. Never guess: ask which picked project it serves, if
/// any, or skip the agent's serena targets. `--yes` cannot answer, so
/// those targets fail.
fn resolve_unknown(
    agent: &AgentCtx,
    picked: &BTreeSet<usize>,
    projects: &[Project],
    ask: bool,
    prompt: &mut dyn Prompt,
) -> Result<UnknownAnswer> {
    let mut a = UnknownAnswer {
        serves: Vec::new(),
        hold: None,
    };
    if picked.is_empty() || agent.existing.unknown.is_empty() {
        return Ok(a);
    }
    if !ask {
        a.hold = Some(Hold::Blocked(format!(
            "serena entr{} {} ha{} no setup record, so the project {} serve{} is unknown; \
             run the wizard in a terminal to say which",
            if agent.existing.unknown.len() == 1 {
                "y"
            } else {
                "ies"
            },
            agent.existing.unknown.join(", "),
            if agent.existing.unknown.len() == 1 {
                "s"
            } else {
                "ve"
            },
            if agent.existing.unknown.len() == 1 {
                "it"
            } else {
                "they"
            },
            if agent.existing.unknown.len() == 1 {
                "s"
            } else {
                ""
            },
        )));
        return Ok(a);
    }
    let order: Vec<usize> = picked.iter().copied().collect();
    for name in &agent.existing.unknown {
        let mut items: Vec<String> = order
            .iter()
            .filter(|i| a.serves.iter().all(|(_, j)| j != *i))
            .map(|i| format!("serves {}", projects[*i].path.display()))
            .collect();
        let free: Vec<usize> = order
            .iter()
            .copied()
            .filter(|i| a.serves.iter().all(|(_, j)| j != i))
            .collect();
        items.push("serves none of these".into());
        items.push(format!("skip serena setup for {}", agent.name));
        let pick = prompt.select(
            &format!(
                "{}: serena entry `{name}` has no setup record. Which project does it serve?",
                agent.name
            ),
            &items,
        )?;
        match answer(pick, &free) {
            Unknown::Serves(pi) => a.serves.push((name.clone(), pi)),
            Unknown::Neither => {}
            Unknown::Skip => {
                a.hold = Some(Hold::Skipped(format!(
                    "skipped: serena entry `{name}` has no setup record"
                )));
                return Ok(a);
            }
        }
    }
    Ok(a)
}

fn answer(pick: usize, free: &[usize]) -> Unknown {
    match pick.checked_sub(free.len()) {
        None => Unknown::Serves(free[pick]),
        Some(0) => Unknown::Neither,
        Some(_) => Unknown::Skip,
    }
}
