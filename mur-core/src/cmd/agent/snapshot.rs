use anyhow::Result;
use std::path::PathBuf;

fn mur_home() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?
        .join(".mur"))
}

/// `mur agent snapshot pull <name>` — assemble the skill snapshot into the
/// agent's `knowledge_cache/` (federation P0). `--dry-run` lists what would
/// be copied under the configured lifecycle floor without writing anything.
pub fn cmd_snapshot_pull(name: &str, dry_run: bool) -> Result<()> {
    let home = mur_home()?;
    if dry_run {
        let cfg = mur_common::config::Config::load_or_default(&home.join("config.yaml"));
        let floor = cfg.federation_snapshot.min_lifecycle;
        let eligible = crate::federation::snapshot::eligible_skills(&home, floor)?;
        println!(
            "Would snapshot {} skills for agent '{name}' (floor: {floor:?}):",
            eligible.len()
        );
        for (skill, state) in &eligible {
            println!("  {skill} ({state:?})");
        }
        return Ok(());
    }
    let snap = crate::federation::assemble_skill_snapshot(&home, name)?;
    println!(
        "Snapshot assembled for '{name}': {} skills @ {}",
        snap.skill_count,
        snap.taken_at.to_rfc3339()
    );
    Ok(())
}

/// `mur agent snapshot show <name>` — print the current skill-snapshot ref.
pub fn cmd_snapshot_show(name: &str) -> Result<()> {
    let home = mur_home()?;
    match crate::federation::read_skill_snapshot_ref(&home, name)? {
        Some(snap) => {
            println!("agent: {name}");
            println!("skills: {}", snap.skill_count);
            println!("taken_at: {}", snap.taken_at.to_rfc3339());
        }
        None => println!("no snapshot for '{name}' (never pulled)"),
    }
    Ok(())
}
