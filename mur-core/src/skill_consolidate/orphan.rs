//! Age-based orphan pass (M5b).
//!
//! Flags skills unused for >180 days that aren't pinned.

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::skill_consolidate::{ConsolidateReport, SkillView};

#[derive(Debug, Clone, serde::Serialize)]
pub struct OrphanFinding {
    pub name: String,
    pub last_used: Option<DateTime<Utc>>,
    pub usage_count: u64,
}

pub fn scan(
    skills: &[SkillView],
    report: &mut ConsolidateReport,
    now: DateTime<Utc>,
) -> Result<()> {
    for s in skills {
        if s.stats.pinned {
            continue;
        }
        if s.stats.usage_count == 0 {
            continue;
        }
        if let Some(last) = s.stats.last_used_at
            && (now - last).num_days() > 180
        {
            report.orphans.push(OrphanFinding {
                name: s.name.clone(),
                last_used: Some(last),
                usage_count: s.stats.usage_count,
            });
        }
    }
    Ok(())
}
