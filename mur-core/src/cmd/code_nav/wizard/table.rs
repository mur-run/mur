//! The combined plan (#2016): one line and one equivalent command per
//! target, the merged install table, new grants grouped by agent, each
//! project's language servers, and the project × agent matrix (display
//! only).

use std::collections::BTreeSet;
use std::io::Write;

use anyhow::Result;

use super::super::consent::{self, tier_label};
use super::super::plan::{InstallRow, LspStatus};
use super::names::Naming;
use super::{Decided, Hold, Spec, sh_quote};

/// Install rows the applying targets need, each tool once.
pub(super) fn shared_installs(d: &Decided) -> Vec<InstallRow> {
    let mut seen = BTreeSet::new();
    d.specs
        .iter()
        .filter(|s| s.applies())
        .flat_map(|s| s.plan.install.iter())
        .filter(|r| r.missing.is_none() && seen.insert((r.name, r.version)))
        .cloned()
        .collect()
}

/// `mur code-nav setup …` that applies `s` alone.
pub(super) fn equivalent(d: &Decided, s: &Spec) -> String {
    let mut c = format!(
        "mur code-nav setup --agent {}",
        sh_quote(&d.agents[s.agent].name)
    );
    if let Some(p) = s.project {
        c.push_str(&format!(
            " --with-serena --project {} --entry {}",
            sh_quote(&d.projects[p].path.display().to_string()),
            s.entry()
        ));
        for l in &s.flags.lsp {
            c.push_str(&format!(" --lsp {l}"));
        }
    }
    c
}

fn describe(d: &Decided, s: &Spec) -> String {
    let agent = &d.agents[s.agent].name;
    let Some(p) = s.project else {
        return format!("{agent}: ast-grep only");
    };
    let path = d.projects[p].path.display();
    let how = match s.naming.as_ref() {
        Some(Naming::Reuse { entry, .. }) if s.diff.is_empty() => {
            format!("reuse {entry} (no change)")
        }
        Some(Naming::Reuse { entry, .. }) => format!("reuse {entry} (already serves {path})"),
        Some(Naming::New {
            entry,
            wanted: Some(w),
        }) => format!("new entry {entry} (`{w}` is taken)"),
        Some(Naming::New { entry, .. }) => format!("new entry {entry}"),
        None => String::new(),
    };
    format!("{agent} → {path}: {how}")
}

pub(super) fn print(d: &Decided, out: &mut dyn Write) -> Result<()> {
    writeln!(out, "Targets:")?;
    for s in &d.specs {
        let line = describe(d, s);
        match &s.hold {
            Some(Hold::Skipped(why)) => writeln!(out, "  - {line} — skipped: {why}")?,
            Some(Hold::Blocked(why)) => writeln!(out, "  ✗ {line} — cannot apply: {why}")?,
            None if s.diff.is_empty() => writeln!(out, "  = {line}")?,
            None => {
                writeln!(out, "  + {line}")?;
                writeln!(out, "      {}", equivalent(d, s))?;
            }
        }
    }
    print_matrix(d, out)?;
    for (ai, entry, dups) in &d.duplicates {
        writeln!(
            out,
            "\n  ! {}: {} also serve the same path as `{entry}`; reusing `{entry}`, nothing is removed",
            d.agents[*ai].name,
            dups.join(", ")
        )?;
    }
    for (ai, entry, path) in &d.kept {
        let a = &d.agents[*ai].name;
        writeln!(
            out,
            "\n  {a}: `{entry}` ({}) not selected — left in place; remove with \
             `mur agent mcp remove {a} {entry}`",
            path.display()
        )?;
    }
    for p in &d.projects {
        let used = d
            .specs
            .iter()
            .any(|s| s.applies() && s.project.is_some_and(|i| d.projects[i].path == p.path));
        if used && !p.indexed {
            writeln!(
                out,
                "\n  {}: not indexed — mur project search won't cover it; run `mur project index {}`",
                p.path.display(),
                sh_quote(&p.path.display().to_string())
            )?;
        }
    }

    let installs = shared_installs(d);
    if installs.is_empty() && !d.specs.iter().any(Spec::applies) {
        return Ok(());
    }
    writeln!(out, "\nInstall (shared, once):")?;
    for r in &installs {
        writeln!(
            out,
            "    {:<12} {:<12} {}",
            r.name,
            r.version,
            r.dir.display()
        )?;
    }
    if installs.is_empty() {
        writeln!(out, "    (nothing)")?;
    }
    for (ai, a) in d.agents.iter().enumerate() {
        let grants: BTreeSet<&String> = d
            .specs
            .iter()
            .filter(|s| s.agent == ai && s.applies())
            .flat_map(|s| s.diff.grants.iter())
            .collect();
        if grants.is_empty() {
            continue;
        }
        writeln!(out, "\nNew permissions for {}:", a.name)?;
        for g in grants {
            writeln!(out, "    {g}")?;
        }
    }
    print_languages(d, out)?;
    for line in d
        .specs
        .iter()
        .filter(|s| s.applies())
        .flat_map(|s| s.plan.disabled_lines())
        .collect::<BTreeSet<_>>()
    {
        writeln!(out, "  ! {line}")?;
    }
    if d.specs.iter().any(|s| s.applies() && s.project.is_some()) {
        writeln!(
            out,
            "\n    also needed after install: exec of serena's (and pyright's) own Python; \
             any lane not granted before is shown by path and asked about then"
        )?;
    }
    Ok(())
}

/// One block per project with an applying target: enabled servers (with
/// their notes), detected opt-in ones left off, and missing prerequisites.
fn print_languages(d: &Decided, out: &mut dyn Write) -> Result<()> {
    let mut shown = BTreeSet::new();
    for s in d.specs.iter().filter(|s| s.applies()) {
        let Some(pi) = s.project else { continue };
        if !shown.insert(pi) {
            continue;
        }
        let p = &d.projects[pi];
        writeln!(out, "\nLanguage servers for {}:", p.path.display())?;
        for r in &s.plan.lsp {
            let detected = p.detected.contains(&r.lang);
            let status = match &r.status {
                LspStatus::Enabled => "enabled".to_owned(),
                LspStatus::Disabled { missing } => {
                    format!("disabled: {missing} not found on PATH — install it and re-run")
                }
                LspStatus::Skipped { .. } if detected => {
                    "detected, off (opt in from the language picker)".to_owned()
                }
                LspStatus::Skipped { .. } => continue,
            };
            writeln!(
                out,
                "    {:<11} {:<19} {status}",
                r.lang.flag(),
                tier_label(r.tier)
            )?;
            if let Some(note) = r.note.filter(|_| r.status == LspStatus::Enabled) {
                writeln!(out, "    {:<11} note: {note}", "")?;
            }
        }
    }
    Ok(())
}

/// Every project some target touches, against every agent.
fn print_matrix(d: &Decided, out: &mut dyn Write) -> Result<()> {
    let rows: BTreeSet<usize> = d.specs.iter().filter_map(|s| s.project).collect();
    if rows.is_empty() || d.agents.len() < 2 {
        return Ok(());
    }
    writeln!(out, "\nProject × agent:")?;
    let w = d
        .agents
        .iter()
        .map(|a| a.name.len())
        .max()
        .unwrap_or(0)
        .max(16);
    write!(out, "    {:<40}", "")?;
    for a in &d.agents {
        write!(out, " {:<w$}", a.name)?;
    }
    writeln!(out)?;
    for pi in rows {
        let p = &d.projects[pi];
        write!(
            out,
            "    {:<40}",
            truncate(&p.path.display().to_string(), 40)
        )?;
        for ai in 0..d.agents.len() {
            let cell = d
                .specs
                .iter()
                .find(|s| s.agent == ai && s.project == Some(pi))
                .map_or("-".to_owned(), |s| match (&s.hold, &s.naming) {
                    (Some(_), _) => "skip".to_owned(),
                    (None, Some(Naming::Reuse { entry, .. })) => entry.clone(),
                    (None, Some(Naming::New { entry, .. })) => format!("+{entry}"),
                    (None, None) => "-".to_owned(),
                });
            write!(out, " {:<w$}", truncate(&cell, w))?;
        }
        writeln!(out)?;
    }
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_owned();
    }
    let tail: String = s
        .chars()
        .rev()
        .take(n - 1)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…{tail}")
}

/// The merged exec-lane question: each lane once, with the agents it goes to.
pub(super) fn print_lanes(
    lanes: &[(std::path::PathBuf, Vec<String>)],
    out: &mut dyn Write,
) -> Result<()> {
    writeln!(out, "\nExec lanes to grant (resolved after install):")?;
    for (l, agents) in lanes {
        writeln!(
            out,
            "    {}  → {}",
            consent::lane_grant_key(l),
            agents.join(", ")
        )?;
    }
    Ok(())
}
