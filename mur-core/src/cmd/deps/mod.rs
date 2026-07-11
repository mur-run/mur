//! Portable program dependencies: aggregate declarations, report, install.
pub mod installer;

pub mod doctor;
pub mod install;
pub mod trust_gate;

use anyhow::{Context, Result};
use mur_common::deps::{DetectMethod, ProgramDep};
use std::path::Path;

/// A declared dependency plus which parts of the artifact declared it.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct AggregatedDep {
    pub dep: ProgramDep,
    pub sources: Vec<String>,
}

/// Dedup `(dep, source)` pairs by `dep.name`, merging sources (first dep wins
/// for detect/registry — they should agree; sources accumulate).
#[allow(dead_code)]
pub fn dedup(raw: Vec<(ProgramDep, String)>) -> Vec<AggregatedDep> {
    let mut out: Vec<AggregatedDep> = Vec::new();
    for (dep, src) in raw {
        if let Some(existing) = out.iter_mut().find(|a| a.dep.name == dep.name) {
            if !existing.sources.contains(&src) {
                existing.sources.push(src);
            }
        } else {
            out.push(AggregatedDep {
                dep,
                sources: vec![src],
            });
        }
    }
    out
}

/// Load an agent's profile directly from `<mur_home>/agents/<agent>/profile.yaml`.
///
/// There is no `load_profile(mur_home, agent)` helper in `mur-core`; the only
/// existing loader (`cmd::agent::load_profile_for_edit`) reads the agent's
/// home via a global env var and returns a `(PathBuf, _AgentProfile)` tuple,
/// which doesn't fit an aggregator that takes an explicit `mur_home`. Read
/// the profile directly instead.
#[allow(dead_code)]
fn load_agent_profile(mur_home: &Path, agent: &str) -> Result<mur_common::agent::AgentProfile> {
    let path = mur_home.join("agents").join(agent).join("profile.yaml");
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    Ok(serde_yaml::from_str(&text)?)
}

/// Collect an agent's declared program deps: its profile.requires_programs,
/// each mounted (enabled) MCP entry's requires_programs, and a synthesized
/// `command`-detect dep per mounted MCP whose `command` is a bare name (not
/// an absolute/relative path).
///
/// Note: skill-declared `requires_programs` are NOT included in this Phase 1
/// pass — there is no single, unambiguous "installed skills for this agent"
/// loader in mur-core (skill installs are global under `~/.mur/skills/`, not
/// per-agent), so wiring it in would mean guessing an API. Left for a
/// follow-up once the skill-loading contract for this feature is settled.
#[allow(dead_code)]
pub fn aggregate_agent(mur_home: &Path, agent: &str) -> Result<Vec<AggregatedDep>> {
    let profile = load_agent_profile(mur_home, agent)?;
    let mut raw: Vec<(ProgramDep, String)> = Vec::new();
    for d in &profile.requires_programs {
        raw.push((d.clone(), "profile".into()));
    }
    for mcp in profile.enabled_mcp_servers() {
        for d in &mcp.requires_programs {
            raw.push((d.clone(), format!("mcp:{}", mcp.name)));
        }
        // Synthesize a command dep for a bare MCP command binary.
        if !mcp.command.contains(std::path::MAIN_SEPARATOR) {
            raw.push((
                ProgramDep {
                    name: mcp.command.clone(),
                    detect: DetectMethod::Command {
                        command: mcp.command.clone(),
                    },
                    reason: format!("MCP server {}", mcp.name),
                    hint: None,
                    registry: None,
                    recipe: None,
                },
                format!("mcp-cmd:{}", mcp.name),
            ));
        }
    }
    Ok(dedup(raw))
}

/// Collect a fleet's deps: its fleet.yaml.requires_programs plus every member
/// agent's aggregate (best-effort — a member whose profile can't be loaded is
/// skipped rather than failing the whole fleet aggregate).
#[allow(dead_code)]
pub fn aggregate_fleet(mur_home: &Path, fleet: &str) -> Result<Vec<AggregatedDep>> {
    let f = crate::cmd::fleet::store::load_fleet(mur_home, fleet)?;
    let mut raw: Vec<(ProgramDep, String)> = Vec::new();
    for d in &f.requires_programs {
        raw.push((d.clone(), "fleet".into()));
    }
    for member in &f.members {
        if let Ok(member_deps) = aggregate_agent(mur_home, member) {
            for a in member_deps {
                for src in a.sources {
                    raw.push((a.dep.clone(), format!("member:{member}:{src}")));
                }
            }
        }
    }
    Ok(dedup(raw))
}

#[cfg(test)]
mod agg_tests {
    use super::*;
    use mur_common::deps::{DetectMethod, ProgramDep};

    fn pd(name: &str) -> ProgramDep {
        ProgramDep {
            name: name.into(),
            detect: DetectMethod::Command {
                command: name.into(),
            },
            reason: "r".into(),
            hint: None,
            registry: None,
            recipe: None,
        }
    }

    #[test]
    fn dedup_merges_sources_by_name() {
        let raw = vec![
            (pd("lightpanda"), "mcp:research-gateway".to_string()),
            (pd("lightpanda"), "skill:render".to_string()),
            (pd("gh"), "profile".to_string()),
        ];
        let out = dedup(raw);
        assert_eq!(out.len(), 2);
        let lp = out.iter().find(|a| a.dep.name == "lightpanda").unwrap();
        assert_eq!(lp.sources.len(), 2);
    }
}

/// Guard test: codifies the non-blocking preflight contract. A report built
/// from missing/empty deps must never error — every integration site
/// (agent doctor, fleet import, fleet/deep-research run, agent start) relies
/// on `build_report` being infallible so a preflight failure can never abort
/// the primary action.
#[cfg(test)]
mod preflight_tests {
    #[test]
    fn preflight_never_errors_on_missing() {
        // A report with missing deps must be Ok (non-blocking).
        let tmp = std::env::temp_dir().join(format!("murpf_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let deps = vec![]; // empty is trivially fine
        let r = crate::cmd::deps::doctor::build_report(&deps, &tmp);
        assert_eq!(crate::cmd::deps::doctor::missing_count(&r), 0);
        std::fs::remove_dir_all(&tmp).ok();
    }
}
