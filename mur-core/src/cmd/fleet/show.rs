//! `mur fleet show` — roster + goal.

use std::path::Path;

use anyhow::Result;

use super::store;

pub fn cmd_fleet_show(mur_home: &Path, name: &str) -> Result<()> {
    let f = store::load_fleet(mur_home, name)?;
    println!("Fleet: {}", f.name);
    println!("Goal: {}", f.goal);
    println!("Router: {}", f.router_or_concierge());
    println!("Members: {}", f.members.join(", "));
    println!("Channel: {}", f.channel_id);
    if super::control::is_stopped(mur_home, name) {
        println!("Status: STOPPED (kill-switch active — `mur fleet start {name}`)");
    }
    if !f.rules.is_empty() {
        println!("Rules: {}", f.rules.join(", "));
    }
    if !f.skills.is_empty() {
        println!("Skills: {}", f.skills.join(", "));
    }

    // Surface the job queue (parity with `mur fleet list`'s JOBS column). Active =
    // not yet terminal (queued/running); terminal jobs stay in `mur fleet jobs --all`.
    let jobs = super::jobs::list_jobs(mur_home, name).unwrap_or_default();
    let active: Vec<_> = jobs.iter().filter(|j| !j.status.is_terminal()).collect();
    if !active.is_empty() {
        println!("Jobs ({} active of {} total):", active.len(), jobs.len());
        for j in &active {
            let short = &j.id[..j.id.len().min(8)];
            println!(
                "  {short}  {}  {}",
                j.status,
                super::list::truncate_goal(&j.text, 60)
            );
        }
    } else if !jobs.is_empty() {
        println!(
            "Jobs: 0 active ({} total) — see `mur fleet jobs {name} --all`",
            jobs.len()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_errors_when_missing_ok_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        assert!(cmd_fleet_show(home, "dev").is_err());
        super::super::create::cmd_fleet_create(home, "dev", vec!["pm".into()], None, None, None).unwrap();
        assert!(cmd_fleet_show(home, "dev").is_ok());
        // With an active job queued, show must still succeed (exercises the jobs branch).
        super::super::jobs::enqueue_job(home, "dev", "do a thing", "cli").unwrap();
        assert!(cmd_fleet_show(home, "dev").is_ok());
    }
}
