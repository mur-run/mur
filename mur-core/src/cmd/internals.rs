//! `mur internals rebuild-index / git` — versioned store maintenance (E1 W3).

use crate::store::versioned::{VersionedYamlStore, agent::VersionedAgentStore};
use anyhow::{Context, Result, bail};

use crate::store::yaml::default_mur_dir;

pub(crate) fn cmd_rebuild_index(layer: &str) -> Result<()> {
    let mur_dir = default_mur_dir();
    match layer {
        "knowledge" => {
            let root = &mur_dir;
            let mut store = if root.join(".git").exists() {
                VersionedYamlStore::open(root).with_context(|| "open knowledge versioned store")?
            } else {
                VersionedYamlStore::init(root).with_context(|| "init knowledge versioned store")?
            };
            store.rebuild_index()?;
            println!("Rebuilt knowledge index at {}", root.display());
        }
        "agents" => {
            let root = mur_dir.join("agents");
            let mut store = if root.join(".git").exists() {
                VersionedAgentStore::open(&root).with_context(|| "open agents versioned store")?
            } else {
                VersionedAgentStore::init(&root).with_context(|| "init agents versioned store")?
            };
            store.rebuild_index()?;
            println!("Rebuilt agents index at {}", root.display());
        }
        other => bail!("Unknown layer '{other}'. Use 'knowledge' or 'agents'."),
    }
    Ok(())
}

/// D3 three-color allowlist for `mur internals git --layer=<layer> <git-args>`.
///
/// White  (log/status/diff/show/fsck) — pass through.
/// Grey   (checkout/stash/reset without --hard) — interactive confirm.
/// Black  (reset --hard/rebase/push) — require `--i-know-what-im-doing`.
pub(crate) fn cmd_internals_git(layer: &str, args: &[String]) -> Result<()> {
    let mur_dir = default_mur_dir();
    let repo_dir = match layer {
        "knowledge" => mur_dir.clone(),
        "agents" => mur_dir.join("agents"),
        other => bail!("Unknown layer '{other}'. Use 'knowledge' or 'agents'."),
    };

    if !repo_dir.join(".git").exists() {
        bail!(
            "Versioned store not initialised at {}. \
             Run `mur internals rebuild-index --layer {layer}` first.",
            repo_dir.display()
        );
    }

    // Strip our sentinel flag before forwarding to git.
    let override_flag = "--i-know-what-im-doing";
    let has_override = args.iter().any(|a| a == override_flag);
    let git_args: Vec<&String> = args
        .iter()
        .filter(|a| a.as_str() != override_flag)
        .collect();

    match classify_git_args(&git_args) {
        Tier::White => {}
        Tier::Grey => {
            let sub = git_args.first().map(|s| s.as_str()).unwrap_or("?");
            eprintln!("⚠️  '{sub}' can modify git history. Confirm? [y/N] ");
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            if !matches!(input.trim().to_lowercase().as_str(), "y" | "yes") {
                bail!("Aborted.");
            }
        }
        Tier::Black => {
            if !has_override {
                let sub = git_args.first().map(|s| s.as_str()).unwrap_or("?");
                bail!(
                    "'{sub}' is a destructive operation.\n\
                     Re-run with `--i-know-what-im-doing` to proceed.\n\
                     Warning: this can permanently rewrite history in the versioned store."
                );
            }
        }
    }

    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo_dir)
        .args(git_args)
        .status()
        .with_context(|| "failed to run git")?;

    if !status.success() {
        bail!("git exited with status {status}");
    }
    Ok(())
}

enum Tier {
    White,
    Grey,
    Black,
}

fn classify_git_args(args: &[&String]) -> Tier {
    let sub = match args.first() {
        Some(s) => s.as_str(),
        None => return Tier::White,
    };
    match sub {
        // White: read-only or safe inspection
        "log" | "status" | "diff" | "show" | "fsck" | "ls-files" | "ls-tree" => Tier::White,
        // Black: destructive
        "rebase" | "push" => Tier::Black,
        "reset" if args.iter().any(|a| a.as_str() == "--hard") => Tier::Black,
        // Grey: potentially modifies state, needs confirmation
        _ => Tier::Grey,
    }
}
