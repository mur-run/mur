//! Propagation candidate enumeration (M7c §3.2).
//!
//! Pure-ish: I/O is bounded to reading peers' manifests + stats. No mutation
//! of any agent state — `run_propagate` (mod.rs) is the only mutator.

use std::path::Path;

use anyhow::Result;
use mur_common::skill::local::list_installed_agent;
use mur_common::skill::peers::list_peer_agents;

use crate::cross_agent::fitness::fitness;
use crate::cross_agent::stats_agg::aggregate_skill_stats;

#[derive(Debug, Clone)]
pub struct Candidate {
    pub source_agent: String,
    pub skill: String,
    pub source_version: String,
    pub population_fitness: f64,
    pub population_samples: u64,
    pub source_agent_weight: f64,
}

#[derive(Debug, Clone)]
pub struct GateConfig {
    pub min_samples: u64,
    pub min_fitness: f64,
    pub min_source_weight: f64,
    pub max_per_sweep: usize,
    pub exclude_patterns: Vec<String>,
    pub half_life_days: u32,
    pub weight_floor: f64,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            min_samples: 5,
            min_fitness: 0.7,
            min_source_weight: 0.3,
            max_per_sweep: 3,
            exclude_patterns: Vec::new(),
            half_life_days: 7,
            weight_floor: 0.1,
        }
    }
}

pub fn enumerate_candidates(
    home: &Path,
    invoking_agent: &str,
    cfg: &GateConfig,
) -> Result<Vec<Candidate>> {
    let peers = list_peer_agents(home)?
        .into_iter()
        .filter(|p| p.name != invoking_agent)
        .collect::<Vec<_>>();

    let now = chrono::Utc::now();
    let local_skills: std::collections::HashSet<String> =
        list_installed_agent(home, invoking_agent)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .into_iter()
            .collect();

    let mut by_skill: std::collections::HashMap<String, Candidate> = Default::default();

    for peer in &peers {
        let weight =
            fitness(home, &peer.name, now, cfg.half_life_days, cfg.weight_floor)?.weight;
        if weight < cfg.min_source_weight {
            continue;
        }
        let skills =
            list_installed_agent(home, &peer.name).map_err(|e| anyhow::anyhow!("{e}"))?;
        for skill in skills {
            if local_skills.contains(&skill) {
                continue;
            }
            if exclude_match(&skill, &cfg.exclude_patterns) {
                continue;
            }
            let agg = aggregate_skill_stats(home, &skill)?;
            let total_usage: u64 = agg.iter().map(|r| r.usage_count).sum();
            if total_usage < cfg.min_samples {
                continue;
            }
            let total_success: u64 = agg.iter().map(|r| r.success_count).sum();
            let total_failure: u64 = agg.iter().map(|r| r.failure_count).sum();
            let pop_fit = if total_success + total_failure > 0 {
                total_success as f64 / (total_success + total_failure) as f64
            } else {
                0.0
            };
            if pop_fit < cfg.min_fitness {
                continue;
            }
            // Source version from this peer's manifest.
            let manifest_path = home
                .join("agents")
                .join(&peer.name)
                .join("skills")
                .join(&skill)
                .join("skill.yaml");
            let version = std::fs::read(&manifest_path)
                .ok()
                .and_then(|bytes| {
                    serde_yaml_ng::from_slice::<mur_common::skill::SkillManifest>(&bytes).ok()
                })
                .map(|m| m.version)
                .unwrap_or_else(|| "0.0.0".into());

            // Per-skill dedupe: pick the peer with the highest per-agent
            // success_rate × weight for this skill (M7c §3.1). On tie,
            // higher peer weight, then alphabetical agent name.
            let per_agent_fit = agg
                .iter()
                .find(|r| r.agent == peer.name)
                .map(|r| {
                    let total = r.success_count + r.failure_count;
                    if total > 0 {
                        r.success_count as f64 / total as f64
                    } else {
                        0.0
                    }
                })
                .unwrap_or(0.0);
            let score = per_agent_fit * weight;
            let cand = Candidate {
                source_agent: peer.name.clone(),
                skill: skill.clone(),
                source_version: version,
                population_fitness: pop_fit,
                population_samples: total_usage,
                source_agent_weight: weight,
            };
            match by_skill.get(&skill) {
                None => {
                    by_skill.insert(skill.clone(), cand);
                }
                Some(existing) => {
                    let existing_score = score_for_existing(existing, &agg);
                    if score > existing_score
                        || (score == existing_score && weight > existing.source_agent_weight)
                        || (score == existing_score
                            && weight == existing.source_agent_weight
                            && peer.name < existing.source_agent)
                    {
                        by_skill.insert(skill.clone(), cand);
                    }
                }
            }
        }
    }

    let mut out: Vec<Candidate> = by_skill.into_values().collect();
    // Sort by population_fitness desc, then agent name asc — determinism.
    out.sort_by(|a, b| {
        b.population_fitness
            .partial_cmp(&a.population_fitness)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.source_agent.cmp(&b.source_agent))
    });
    if out.len() > cfg.max_per_sweep {
        out.truncate(cfg.max_per_sweep);
    }
    Ok(out)
}

fn score_for_existing(
    existing: &Candidate,
    agg: &[crate::cross_agent::stats_agg::AgentSkillStats],
) -> f64 {
    let per_agent = agg
        .iter()
        .find(|r| r.agent == existing.source_agent)
        .map(|r| {
            let total = r.success_count + r.failure_count;
            if total > 0 {
                r.success_count as f64 / total as f64
            } else {
                0.0
            }
        })
        .unwrap_or(0.0);
    per_agent * existing.source_agent_weight
}

fn exclude_match(skill: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| glob_match(p, skill))
}

/// Tiny glob matcher: `*` matches zero-or-more characters, `?` exactly one.
fn glob_match(pattern: &str, input: &str) -> bool {
    fn rec(p: &[u8], i: &[u8]) -> bool {
        match (p.first(), i.first()) {
            (None, None) => true,
            (Some(b'*'), _) => rec(&p[1..], i) || (!i.is_empty() && rec(p, &i[1..])),
            (Some(b'?'), Some(_)) => rec(&p[1..], &i[1..]),
            (Some(pc), Some(ic)) if pc == ic => rec(&p[1..], &i[1..]),
            _ => false,
        }
    }
    rec(pattern.as_bytes(), input.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_prefix_star() {
        assert!(glob_match("secrets-*", "secrets-aws"));
        assert!(glob_match("secrets-*", "secrets-"));
        assert!(!glob_match("secrets-*", "public-aws"));
    }

    #[test]
    fn glob_handles_question_mark() {
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "abbc"));
    }

    #[test]
    fn exclude_match_any_pattern() {
        let pats = vec!["secrets-*".into(), "tmp-*".into()];
        assert!(exclude_match("secrets-aws", &pats));
        assert!(exclude_match("tmp-foo", &pats));
        assert!(!exclude_match("research-prices", &pats));
    }

    #[test]
    fn default_gates_are_strict() {
        let g = GateConfig::default();
        assert_eq!(g.min_samples, 5);
        assert!((g.min_fitness - 0.7).abs() < 1e-9);
        assert!((g.min_source_weight - 0.3).abs() < 1e-9);
        assert_eq!(g.max_per_sweep, 3);
    }
}
