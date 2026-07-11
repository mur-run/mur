//! Read-only report: what declared programs are present/missing, and how to get them.

use crate::cmd::deps::AggregatedDep;
use mur_common::deps::DepStatus;
use mur_common::deps::detect::detect;
use mur_common::deps::registry::is_curated;
use std::path::Path;

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub enum Tier {
    /// Installable via `install-deps` from MUR's curated registry.
    Curated,
    /// Detect-and-guide only (unknown/untrusted source) — Phase 1.
    Manual,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct DepReportLine {
    pub name: String,
    pub reason: String,
    pub status: DepStatus,
    pub tier: Tier,
    pub hint: Option<String>,
    pub sources: Vec<String>,
}

/// Detect each dep and classify its install tier.
#[allow(dead_code)]
pub fn build_report(deps: &[AggregatedDep], mur_home: &Path) -> Vec<DepReportLine> {
    deps.iter()
        .map(|a| {
            let key = a.dep.registry.as_deref().unwrap_or(&a.dep.name);
            let tier = if is_curated(key) {
                Tier::Curated
            } else {
                Tier::Manual
            };
            DepReportLine {
                name: a.dep.name.clone(),
                reason: a.dep.reason.clone(),
                status: detect(&a.dep, mur_home),
                tier,
                hint: a.dep.hint.clone(),
                sources: a.sources.clone(),
            }
        })
        .collect()
}

#[allow(dead_code)]
pub fn missing_count(lines: &[DepReportLine]) -> usize {
    lines
        .iter()
        .filter(|l| l.status != DepStatus::Present)
        .count()
}

/// Print the human report. `install_cmd` is the exact `... install-deps <name>`
/// the caller should run for curated deps.
#[allow(dead_code)]
pub fn print_report(lines: &[DepReportLine], install_cmd: &str) {
    if lines.is_empty() {
        println!("No external program dependencies declared.");
        return;
    }
    println!("External program dependencies:");
    for l in lines {
        let mark = match l.status {
            DepStatus::Present => "\u{2713}",
            _ => "\u{2717}",
        };
        let tier = match l.tier {
            Tier::Curated => "[curated]",
            Tier::Manual => "[manual]",
        };
        println!("  {mark} {:<16} {}   {tier}", l.name, l.reason);
        if l.status != DepStatus::Present {
            if matches!(l.tier, Tier::Curated) {
                println!("      auto:   {install_cmd}");
            }
            if let Some(h) = &l.hint {
                println!("      manual: {h}");
            }
        }
    }
    let missing = missing_count(lines);
    if missing > 0 {
        println!("{missing} missing — the artifact runs without them (features degrade).");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::deps::AggregatedDep;
    use mur_common::deps::{DetectMethod, ProgramDep};

    fn agg(name: &str, registry: Option<&str>, detect_file: &str) -> AggregatedDep {
        AggregatedDep {
            dep: ProgramDep {
                name: name.into(),
                detect: DetectMethod::File {
                    file: detect_file.into(),
                },
                reason: "render".into(),
                hint: Some("http://x".into()),
                registry: registry.map(|s| s.into()),
                recipe: None,
            },
            sources: vec!["mcp:gw".into()],
        }
    }

    #[test]
    fn report_marks_missing_and_tier() {
        let tmp = std::env::temp_dir().join(format!("murdoc_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        // lightpanda is curated + missing (file absent under tmp)
        let deps = vec![agg("lightpanda", Some("lightpanda"), "aura/lightpanda")];
        let lines = build_report(&deps, &tmp);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].status, mur_common::deps::DepStatus::Missing);
        assert!(matches!(lines[0].tier, Tier::Curated));
        assert_eq!(missing_count(&lines), 1);
        std::fs::remove_dir_all(&tmp).ok();
    }
}
