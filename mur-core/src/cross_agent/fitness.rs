//! Per-agent fitness scoring with half-life decay (M7a Task 4).

use chrono::{DateTime, Duration, Utc};
use mur_common::skill::local::list_installed_agent;
use mur_common::skill::stats::SkillStats;
use std::path::Path;

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentFitness {
    pub agent: String,
    /// decayed_success_rate * recency_decay
    pub weight: f64,
    /// success / (success + failure), or 0 if no samples
    pub success_rate: f64,
    /// total usage_count across this agent's skills
    pub sample_size: u64,
    pub last_seen: Option<DateTime<Utc>>,
    /// ∈ [floor, 1.0]
    pub recency_decay: f64,
}

pub fn fitness(
    home: &Path,
    agent: &str,
    now: DateTime<Utc>,
    half_life_days: u32,
    floor: f64,
) -> anyhow::Result<AgentFitness> {
    let mut sample_size = 0u64;
    let mut success_total = 0u64;
    let mut failure_total = 0u64;
    let mut latest: Option<DateTime<Utc>> = None;

    for skill in list_installed_agent(home, agent).map_err(|e| anyhow::anyhow!("{e}"))? {
        let path = SkillStats::path_agent(home, agent, &skill);
        if !path.exists() {
            continue;
        }
        let Some(stats) = SkillStats::load(&path)? else {
            continue;
        };
        sample_size += stats.usage_count;
        success_total += stats.success_count;
        failure_total += stats.failure_count;
        if let Some(t) = stats.last_used_at {
            latest = Some(latest.map_or(t, |prev| prev.max(t)));
        }
    }

    let success_rate = if success_total + failure_total > 0 {
        success_total as f64 / (success_total + failure_total) as f64
    } else {
        0.0
    };

    let recency_decay = match latest {
        Some(t) => decay_factor(now - t, half_life_days, floor),
        None => floor,
    };

    Ok(AgentFitness {
        agent: agent.to_string(),
        weight: success_rate * recency_decay,
        success_rate,
        sample_size,
        last_seen: latest,
        recency_decay,
    })
}

pub fn decay_factor(elapsed: Duration, half_life_days: u32, floor: f64) -> f64 {
    let days = elapsed.num_seconds() as f64 / 86_400.0;
    if days < 0.0 {
        return 1.0;
    }
    let raw = 0.5_f64.powf(days / half_life_days as f64);
    raw.max(floor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decay_at_one_half_life_is_half() {
        let v = decay_factor(Duration::days(7), 7, 0.1);
        assert!((v - 0.5).abs() < 1e-6);
    }

    #[test]
    fn decay_at_zero_is_one() {
        assert_eq!(decay_factor(Duration::zero(), 7, 0.1), 1.0);
    }

    #[test]
    fn decay_floors_at_long_offline() {
        let v = decay_factor(Duration::days(365), 7, 0.1);
        assert_eq!(v, 0.1);
    }

    #[test]
    fn negative_elapsed_treated_as_present() {
        let v = decay_factor(Duration::seconds(-3600), 7, 0.1);
        assert_eq!(v, 1.0);
    }
}
