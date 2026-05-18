//! `mur pattern history/diff/rollback` — versioned pattern commands (E1 W3).

use crate::store::versioned::VersionedYamlStore;
use anyhow::{Context, Result};
use std::path::Path;

use crate::store::yaml::default_mur_dir;

// ── history ───────────────────────────────────────────────────────────────────

pub(crate) fn cmd_pattern_history(name: &str) -> Result<()> {
    let root = default_mur_dir();
    let store = open_or_init(&root)?;

    let hist = store.history(name)?;
    if hist.is_empty() {
        println!("No version history for '{name}'.");
        println!("(History is recorded starting from the first save after versioned store init.)");
        return Ok(());
    }

    println!("History for pattern '{name}':");
    println!("{:<5} {:<14} {:<26} reason", "v", "sha", "timestamp");
    println!("{}", "-".repeat(70));
    for e in &hist {
        let ts = chrono::DateTime::from_timestamp(e.timestamp, 0)
            .map(|dt: chrono::DateTime<chrono::Utc>| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
            .unwrap_or_else(|| "unknown".to_string());
        println!("{:<5} {:<14} {:<26} {}", e.version, e.sha, ts, e.reason);
    }
    Ok(())
}

// ── diff ──────────────────────────────────────────────────────────────────────

pub(crate) fn cmd_pattern_diff(name: &str, v1: Option<u32>, v2: Option<u32>) -> Result<()> {
    let root = default_mur_dir();
    let store = open_or_init(&root)?;

    let hist = store.history(name)?;
    if hist.len() < 2 {
        println!("Pattern '{name}' has fewer than 2 versions — nothing to diff.");
        return Ok(());
    }

    // Resolve default versions: v1 = previous, v2 = current
    let current_v = store.current_version(name);
    let v2 = v2.unwrap_or(current_v);
    let v1 = v1.unwrap_or_else(|| v2.saturating_sub(1).max(1));

    let content_a = read_version(&root, name, v1, current_v)?;
    let content_b = read_version(&root, name, v2, current_v)?;

    if content_a == content_b {
        println!("v{v1} and v{v2} are identical.");
        return Ok(());
    }

    println!("Diff for pattern '{name}' (v{v1} → v{v2}):");
    println!("{}", "-".repeat(60));
    print_diff(&content_a, &content_b);
    Ok(())
}

// ── rollback ──────────────────────────────────────────────────────────────────

pub(crate) fn cmd_pattern_rollback(name: &str, to: u32) -> Result<()> {
    let root = default_mur_dir();
    let mut store = open_or_init(&root)?;

    let current_v = store.current_version(name);
    if current_v == 0 {
        anyhow::bail!("Pattern '{name}' has no version history — cannot rollback.");
    }
    if to >= current_v {
        anyhow::bail!(
            "Cannot rollback to v{to}: current version is v{current_v}. \
             Choose a version less than v{current_v}."
        );
    }

    let rev = store.rollback_pattern(name, to)?;
    println!(
        "Rolled back '{name}' to v{to} → new commit v{} ({})",
        rev.version, rev.sha
    );
    Ok(())
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn open_or_init(root: &Path) -> Result<VersionedYamlStore> {
    if root.join(".git").exists() {
        VersionedYamlStore::open(root)
            .with_context(|| format!("open versioned store at {}", root.display()))
    } else {
        VersionedYamlStore::init(root)
            .with_context(|| format!("init versioned store at {}", root.display()))
    }
}

/// Return the YAML content for a given version of a pattern.
/// `current_v` is the live version so we know whether to read from disk or archive.
fn read_version(root: &Path, name: &str, version: u32, current_v: u32) -> Result<String> {
    if version == current_v {
        let path = root.join("patterns").join(format!("{name}.yaml"));
        return std::fs::read_to_string(&path)
            .with_context(|| format!("read current pattern file for '{name}'"));
    }
    let archive = root
        .join("archive/patterns")
        .join(name)
        .join(format!("v{version}.yaml"));
    std::fs::read_to_string(&archive)
        .with_context(|| format!("read archive v{version} for '{name}'"))
}

fn print_diff(a: &str, b: &str) {
    for diff in diff::lines(a, b) {
        match diff {
            diff::Result::Left(l) => println!("- {l}"),
            diff::Result::Right(r) => println!("+ {r}"),
            diff::Result::Both(l, _) => println!("  {l}"),
        }
    }
}
