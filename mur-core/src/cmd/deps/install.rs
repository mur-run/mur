//! `install-deps`: install missing CURATED deps (consent-gated). Manual-only
//! deps are never installed — their hint is printed.

use crate::cmd::deps::doctor::{DepReportLine, Tier};
use anyhow::{Context, Result};
use mur_common::deps::registry::recipe;
use mur_common::deps::{DepStatus, current_platform};
use std::path::Path;

/// Names of deps that are missing, curated, and (if `only` set) match it.
pub fn installable(lines: &[DepReportLine], only: Option<&str>) -> Vec<String> {
    lines
        .iter()
        .filter(|l| l.status != DepStatus::Present && matches!(l.tier, Tier::Curated))
        .filter(|l| only.is_none_or(|o| o == l.name))
        .map(|l| l.name.clone())
        .collect()
}

/// Prompt y/N unless `yes`. Returns true to proceed.
fn confirm(prompt: &str, yes: bool) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    use std::io::Write;
    print!("{prompt} [y/N] ");
    std::io::stdout().flush().ok();
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .context("read stdin")?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// Install each installable dep after per-item consent (unless `yes`).
pub async fn cmd_install_deps(
    mur_home: &Path,
    lines: &[DepReportLine],
    only: Option<&str>,
    yes: bool,
) -> Result<()> {
    let names = installable(lines, only);
    if names.is_empty() {
        println!("Nothing to install (no missing curated deps match).");
        for l in lines
            .iter()
            .filter(|l| l.status != DepStatus::Present && matches!(l.tier, Tier::Manual))
        {
            if let Some(h) = &l.hint {
                println!("  {} — install manually: {h}", l.name);
            }
        }
        return Ok(());
    }
    let platform = current_platform();
    for name in names {
        let Some(rec) = recipe(&name, &platform) else {
            println!("  {name}: no recipe for platform {platform} — skipping (install manually).");
            continue;
        };
        if !confirm(&format!("Install {name} from {} ?", rec.url), yes)? {
            println!("  skipped {name}.");
            continue;
        }
        match crate::cmd::deps::installer::install(&rec, mur_home).await {
            Ok(paths) => println!("  installed {name} -> {paths:?}"),
            Err(e) => println!("  FAILED {name}: {e}"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::deps::doctor::{DepReportLine, Tier};
    use mur_common::deps::DepStatus;

    fn line(name: &str, tier: Tier, status: DepStatus) -> DepReportLine {
        DepReportLine {
            name: name.into(),
            reason: "r".into(),
            status,
            tier,
            hint: Some("h".into()),
            sources: vec![],
        }
    }

    #[test]
    fn selects_only_missing_curated_respecting_program_filter() {
        let lines = vec![
            line("lightpanda", Tier::Curated, DepStatus::Missing),
            line("obscura", Tier::Curated, DepStatus::Present), // present → skip
            line("weirdtool", Tier::Manual, DepStatus::Missing), // manual → skip
        ];
        assert_eq!(installable(&lines, None), vec!["lightpanda"]);
        assert_eq!(installable(&lines, Some("obscura")), Vec::<String>::new()); // present
        assert_eq!(installable(&lines, Some("weirdtool")), Vec::<String>::new()); // manual
    }
}
