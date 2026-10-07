//! `mur agent turn {list,undo}` — per-turn undo over the snapshot the
//! runtime writes at promote (design spec §4.1 step 4).
//!
//! Undo is last-write-wins in reverse and P0 has no `before_hash` check, so
//! it says what it is about to overwrite *before* it does: every path whose
//! bytes moved on after the promote is listed and the user confirms. An
//! undone turn cannot be undone again (the manifest is marked).

use std::io::{BufRead, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use mur_track::{EntryKind, TurnManifest, UndoStore};

use super::resolve_mur_home;

fn store_for(name: &str) -> Result<(String, UndoStore)> {
    let home = resolve_mur_home()?;
    let canonical = crate::a2a_dial::canonicalize_agent_name(&home, name);
    let agent_home = home.join("agents").join(&canonical);
    if !agent_home.join("profile.yaml").is_file() {
        bail!(
            "agent '{name}' not found under {}",
            home.join("agents").display()
        );
    }
    Ok((canonical, UndoStore::new(&agent_home)))
}

/// `mur agent turn list <agent>`: every promoted turn with a snapshot,
/// newest first.
pub fn cmd_turn_list(name: &str, json: bool) -> Result<()> {
    let (canonical, store) = store_for(name)?;
    let turns = store.list()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&turns)?);
        return Ok(());
    }
    if turns.is_empty() {
        println!("no undoable turns for agent '{canonical}'");
        return Ok(());
    }
    println!(
        "{:<44} {:<25} {:<7} {:<6} PROJECT",
        "TURN", "PROMOTED", "FILES", "STATE"
    );
    for m in &turns {
        let state = if m.undone_at.is_some() { "undone" } else { "-" };
        println!(
            "{:<44} {:<25} {:<7} {:<6} {}",
            m.turn,
            m.promoted_at,
            m.restorable().count(),
            state,
            m.project.display()
        );
    }
    Ok(())
}

/// `mur agent turn undo <agent> <turn> [--dry-run] [--yes]`.
pub fn cmd_turn_undo(name: &str, turn: &str, dry_run: bool, yes: bool) -> Result<()> {
    let (canonical, store) = store_for(name)?;
    let Some(mut m) = store.load(turn)? else {
        bail!(
            "no undo snapshot for turn '{turn}' of agent '{canonical}' — \
             see `mur agent turn list {canonical}`"
        );
    };
    if let Some(at) = &m.undone_at {
        bail!("turn '{turn}' was already undone at {at}; an undo cannot be undone (P1)");
    }
    let drift = store.drift(&m)?;
    print_plan(&m, &drift);
    if dry_run {
        println!("dry run — nothing written");
        return Ok(());
    }
    if !yes && !confirm(&drift)? {
        println!("aborted");
        return Ok(());
    }
    let report = store.undo(&mut m)?;
    println!(
        "undone turn '{}': {} restored, {} removed, {} skipped",
        m.turn,
        report.restored.len(),
        report.removed.len(),
        report.skipped.len()
    );
    Ok(())
}

fn print_plan(m: &TurnManifest, drift: &[mur_track::Drift]) {
    println!(
        "turn {} promoted {} into {}",
        m.turn,
        m.promoted_at,
        m.project.display()
    );
    for e in &m.entries {
        let verb = match e.kind {
            EntryKind::Modified | EntryKind::Deleted => "restore",
            EntryKind::Added => "remove ",
            EntryKind::Skipped => "skip   ",
        };
        let note = match e.kind {
            EntryKind::Skipped => e.reason.clone().unwrap_or_default(),
            _ if drift.iter().any(|d| d.path == e.path) => "CHANGED SINCE PROMOTE".to_string(),
            _ => String::new(),
        };
        println!("  {verb} {}  {note}", display_rel(&e.path));
    }
    if !drift.is_empty() {
        println!(
            "warning: {} path(s) changed after this turn was promoted; undo overwrites \
             those changes (P0 has no conflict check)",
            drift.len()
        );
    }
}

fn display_rel(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn confirm(drift: &[mur_track::Drift]) -> Result<bool> {
    let prompt = if drift.is_empty() {
        "undo this turn? [y/N] "
    } else {
        "overwrite the changed paths and undo this turn? [y/N] "
    };
    let mut out = std::io::stdout();
    out.write_all(prompt.as_bytes())?;
    out.flush()?;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .context("read confirmation")?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}
