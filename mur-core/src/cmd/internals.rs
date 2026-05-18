//! `mur internals rebuild-index / git` — versioned store maintenance (E1 W3).

use anyhow::{Context, Result};
use crate::store::versioned::{VersionedYamlStore, agent::VersionedAgentStore};

use crate::store::yaml::default_mur_dir;

pub(crate) fn cmd_rebuild_index(layer: &str) -> Result<()> {
    let mur_dir = default_mur_dir();
    match layer {
        "knowledge" => {
            let root = &mur_dir;
            let mut store = if root.join(".git").exists() {
                VersionedYamlStore::open(root)
                    .with_context(|| "open knowledge versioned store")?
            } else {
                VersionedYamlStore::init(root)
                    .with_context(|| "init knowledge versioned store")?
            };
            store.rebuild_index()?;
            println!("Rebuilt knowledge index at {}", root.display());
        }
        "agents" => {
            let root = mur_dir.join("agents");
            let mut store = if root.join(".git").exists() {
                VersionedAgentStore::open(&root)
                    .with_context(|| "open agents versioned store")?
            } else {
                VersionedAgentStore::init(&root)
                    .with_context(|| "init agents versioned store")?
            };
            store.rebuild_index()?;
            println!("Rebuilt agents index at {}", root.display());
        }
        other => anyhow::bail!("Unknown layer '{other}'. Use 'knowledge' or 'agents'."),
    }
    Ok(())
}

pub(crate) fn cmd_internals_git(layer: &str, args: &[String]) -> Result<()> {
    let mur_dir = default_mur_dir();
    let repo_dir = match layer {
        "knowledge" => mur_dir.clone(),
        "agents" => mur_dir.join("agents"),
        other => anyhow::bail!("Unknown layer '{other}'. Use 'knowledge' or 'agents'."),
    };

    if !repo_dir.join(".git").exists() {
        anyhow::bail!(
            "Versioned store not initialised at {}. \
             Run `mur internals rebuild-index --layer {layer}` first.",
            repo_dir.display()
        );
    }

    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo_dir)
        .args(args)
        .status()
        .with_context(|| "failed to run git")?;

    if !status.success() {
        anyhow::bail!("git exited with status {status}");
    }
    Ok(())
}
