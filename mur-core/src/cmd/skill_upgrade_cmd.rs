//! `mur skill upgrade [--check] [--json]` — CLI surface over
//! `skill_upgrade::upgrade_all`.

use anyhow::{Context, Result};

use crate::cmd::agent::resolve_mur_home;
use crate::cmd::skill_registry;
use crate::cmd::skill_upgrade::{UpgradeReport, UpgradeStatus, upgrade_all};

/// Human-readable summary: one line per item, plus a totals line.
pub fn format_report(report: &UpgradeReport) -> String {
    let mut lines = Vec::new();
    let mut upgraded = 0usize;
    let mut up_to_date = 0usize;
    let mut blocked = 0usize;
    let mut not_in_registry = 0usize;
    let mut errors = 0usize;

    for item in &report.items {
        let (word, detail) = match &item.status {
            UpgradeStatus::UpToDate => {
                up_to_date += 1;
                ("up-to-date".to_string(), String::new())
            }
            UpgradeStatus::Upgraded { from, to } => {
                upgraded += 1;
                ("upgraded".to_string(), format!(" ({from} -> {to})"))
            }
            UpgradeStatus::BlockedModified { local, latest } => {
                blocked += 1;
                (
                    "blocked (modified locally)".to_string(),
                    format!(" (installed {local}, latest {latest})"),
                )
            }
            UpgradeStatus::NotInRegistry => {
                not_in_registry += 1;
                ("not in registry".to_string(), String::new())
            }
            UpgradeStatus::Error(e) => {
                errors += 1;
                ("error".to_string(), format!(": {e}"))
            }
        };
        lines.push(format!("{}: {word}{detail}", item.name));
    }

    lines.push(format!(
        "{} upgraded, {} up-to-date, {} blocked, {} not in registry, {} errors",
        upgraded, up_to_date, blocked, not_in_registry, errors
    ));
    lines.join("\n")
}

/// CLI shim — resolves MUR_HOME + registry URL from env, fetches the
/// registry, runs the upgrade pass, and prints the report.
pub fn cmd_upgrade_cli(check: bool, json: bool) -> Result<()> {
    let home = resolve_mur_home()?;
    let registry_url = std::env::var("MUR_SKILL_REGISTRY_URL")
        .unwrap_or_else(|_| skill_registry::DEFAULT_REGISTRY.to_string());
    let registry_dir =
        skill_registry::fetch_registry(&home, &registry_url).context("fetch skill registry")?;

    let report = upgrade_all(&home, &registry_dir, !check);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("serialize upgrade report")?
        );
    } else {
        println!("{}", format_report(&report));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::skill_upgrade::UpgradeItem;
    use std::path::PathBuf;

    #[test]
    fn formats_one_line_per_item_and_summary() {
        let report = UpgradeReport {
            items: vec![
                UpgradeItem {
                    name: "a".into(),
                    dir: PathBuf::from("/tmp/a"),
                    status: UpgradeStatus::Upgraded {
                        from: "1.0.0".into(),
                        to: "1.1.0".into(),
                    },
                },
                UpgradeItem {
                    name: "b".into(),
                    dir: PathBuf::from("/tmp/b"),
                    status: UpgradeStatus::UpToDate,
                },
                UpgradeItem {
                    name: "c".into(),
                    dir: PathBuf::from("/tmp/c"),
                    status: UpgradeStatus::BlockedModified {
                        local: "1.0.0".into(),
                        latest: "1.2.0".into(),
                    },
                },
                UpgradeItem {
                    name: "d".into(),
                    dir: PathBuf::from("/tmp/d"),
                    status: UpgradeStatus::NotInRegistry,
                },
                UpgradeItem {
                    name: "e".into(),
                    dir: PathBuf::from("/tmp/e"),
                    status: UpgradeStatus::Error("boom".into()),
                },
            ],
        };
        let out = format_report(&report);
        let mut lines = out.lines();
        assert_eq!(lines.next().unwrap(), "a: upgraded (1.0.0 -> 1.1.0)");
        assert_eq!(lines.next().unwrap(), "b: up-to-date");
        assert_eq!(
            lines.next().unwrap(),
            "c: blocked (modified locally) (installed 1.0.0, latest 1.2.0)"
        );
        assert_eq!(lines.next().unwrap(), "d: not in registry");
        assert_eq!(lines.next().unwrap(), "e: error: boom");
        assert_eq!(
            lines.next().unwrap(),
            "1 upgraded, 1 up-to-date, 1 blocked, 1 not in registry, 1 errors"
        );
        assert!(lines.next().is_none());
    }

    #[test]
    fn empty_report_summary_line() {
        let report = UpgradeReport { items: vec![] };
        assert_eq!(
            format_report(&report),
            "0 upgraded, 0 up-to-date, 0 blocked, 0 not in registry, 0 errors"
        );
    }
}
