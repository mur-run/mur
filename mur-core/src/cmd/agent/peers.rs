//! `mur agent peers` CLI handler (M7a).

use std::path::Path;

use anyhow::Result;
use mur_common::skill::peers::list_peer_agents;

pub fn cmd_peers(home: &Path, json: bool) -> Result<()> {
    let peers = list_peer_agents(home)?;
    if json {
        serde_json::to_writer_pretty(
            std::io::stdout(),
            &peers
                .iter()
                .map(|p| {
                    serde_json::json!({
                        "name": p.name,
                        "home_path": p.home_path,
                        "skills_count": p.skills_count,
                    })
                })
                .collect::<Vec<_>>(),
        )?;
        println!();
        return Ok(());
    }
    if peers.is_empty() {
        println!("No peer agents found.");
        return Ok(());
    }
    println!("{:<24} {:>8}  HOME", "AGENT", "SKILLS");
    for p in &peers {
        println!(
            "{:<24} {:>8}  {}",
            p.name,
            p.skills_count,
            p.home_path.display()
        );
    }
    Ok(())
}
