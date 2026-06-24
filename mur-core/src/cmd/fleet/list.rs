//! `mur fleet list` — all fleets.

use std::path::Path;

use anyhow::Result;
use mur_common::fleet::JobStatus;

use super::{control, jobs, store};

fn status_symbol(stopped: bool, running: bool) -> &'static str {
    if stopped {
        "⏸" // stopped wins over running
    } else if running {
        "▶"
    } else {
        "●" // idle
    }
}

fn truncate_goal(goal: &str, width: usize) -> String {
    // Collapse all whitespace/newlines to single spaces
    let collapsed = goal.split_whitespace().collect::<Vec<_>>().join(" ");

    // Truncate to width chars with trailing …
    if collapsed.chars().count() > width {
        collapsed
            .chars()
            .take(width.saturating_sub(1))
            .collect::<String>()
            + "…"
    } else {
        collapsed
    }
}

fn terminal_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse::<usize>().ok())
        .filter(|w| *w >= 40)
        .unwrap_or(80)
}

pub fn cmd_fleet_list(mur_home: &Path) -> Result<()> {
    let names = store::list_fleets(mur_home)?;
    if names.is_empty() {
        println!("No fleets. Create one: mur fleet create <name> --members a,b,c --goal \"...\"");
        return Ok(());
    }

    // Gather rows first to compute column widths based on data
    struct Row {
        name: String,
        st: &'static str,
        mem: usize,
        queued: usize,
        router: String,
        goal: String,
    }
    let mut rows = Vec::new();
    for n in &names {
        let f = store::load_fleet(mur_home, n)?;
        let job_list = jobs::list_jobs(mur_home, n).unwrap_or_default();
        let running = job_list.iter().any(|j| j.status == JobStatus::Running);
        let queued = job_list
            .iter()
            .filter(|j| j.status == JobStatus::Queued)
            .count();
        rows.push(Row {
            name: f.name.clone(),
            st: status_symbol(control::is_stopped(mur_home, n), running),
            mem: f.members.len(),
            queued,
            router: f.router_or_concierge().to_string(),
            goal: f.goal.clone(),
        });
    }

    // Compute column widths: NAME and ROUTER are variable-width,
    // ST (2), MEM (4), JOBS (4) are fixed-width columns
    let name_w = rows
        .iter()
        .map(|r| r.name.chars().count())
        .max()
        .unwrap_or(4)
        .max(4); // "NAME" header width
    let router_w = rows
        .iter()
        .map(|r| r.router.chars().count())
        .max()
        .unwrap_or(6)
        .max(6); // "ROUTER" header width

    // GOAL gets terminal remainder
    // 2-space gaps: ST(2) + MEM(4) + JOBS(4) = 10 fixed-width cols
    let fixed = name_w + router_w + 10 + 10;
    let term_w = terminal_width();
    let goal_w = term_w.saturating_sub(fixed).max(20);

    // Print header
    println!(
        "{:<name_w$}  ST  MEM  JOBS  {:<router_w$}  GOAL",
        "NAME",
        "ROUTER",
        name_w = name_w,
        router_w = router_w
    );

    // Print separator
    println!(
        "{}  --  ---  ----  {}  {}",
        "─".repeat(name_w),
        "─".repeat(router_w),
        "─".repeat(goal_w)
    );

    // Print rows
    for row in &rows {
        let goal_display = truncate_goal(&row.goal, goal_w);
        println!(
            "{:<name_w$}  {}  {:<3}  {:<4}  {:<router_w$}  {}",
            row.name,
            row.st,
            row.mem,
            row.queued,
            row.router,
            goal_display,
            name_w = name_w,
            router_w = router_w
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_symbol_precedence() {
        assert_eq!(status_symbol(true, true), "⏸"); // stopped wins over running
        assert_eq!(status_symbol(true, false), "⏸");
        assert_eq!(status_symbol(false, true), "▶");
        assert_eq!(status_symbol(false, false), "●");
    }

    #[test]
    fn truncate_goal_collapses_newlines_and_caps_width() {
        assert_eq!(truncate_goal("a\nb\nc", 80), "a b c");
        let long = "x".repeat(100);
        let out = truncate_goal(&long, 10);
        assert!(
            out.chars().count() <= 10,
            "got {} chars",
            out.chars().count()
        );
        assert!(out.ends_with('…'));
    }
}
