//! Repair impl for `execution-recency` sidecar findings.
//!
//! A missing or unreadable stats sidecar is a cache miss — the JSONL trace
//! log is the source of truth — so the repair is a rebuild via
//! `reindex_stats`, exactly what the finding's remediation tells a human
//! to run by hand.

use crate::cmd::skill_doctor::Finding;
use crate::skill_repair::{Repair, RepairCtx, RepairOutcome};
use crate::skill_stats::reindex::{DEFAULT_DAYS_BACK, ReindexOptions, reindex_stats};

pub struct StatsSidecarRepair;

impl Repair for StatsSidecarRepair {
    fn check_id(&self) -> &'static str {
        "execution-recency"
    }

    fn applicable(&self, finding: &Finding) -> bool {
        finding.check_id == "execution-recency" && finding.fixable
    }

    fn run(&self, finding: &Finding, ctx: &RepairCtx, apply: bool) -> RepairOutcome {
        if !apply {
            return RepairOutcome::DryRun(format!(
                "would rebuild stats sidecar for {} from traces",
                finding.skill_name
            ));
        }
        let opts = ReindexOptions {
            skill_filter: Some(finding.skill_name.clone()),
            since: None,
            days_back: DEFAULT_DAYS_BACK,
        };
        match reindex_stats(ctx.home, opts) {
            Ok(_) => RepairOutcome::Fixed,
            Err(e) => RepairOutcome::Failed(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::skill_doctor::{Finding, Severity};
    use mur_common::skill::stats::SkillStats;

    fn sidecar_finding(skill: &str) -> Finding {
        Finding {
            check_id: "execution-recency".into(),
            category: "recency".into(),
            severity: Severity::Unknown,
            skill_name: skill.into(),
            message: "No stats sidecar — run `mur skill reindex-stats` to rebuild.".into(),
            remediation: Some(format!("mur skill reindex-stats {skill}")),
            fixable: true,
        }
    }

    #[test]
    fn rebuilds_missing_sidecar_from_traces() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join("skills").join("my-skill")).unwrap();
        std::fs::write(
            home.join("skills").join("my-skill").join("skill.yaml"),
            "name: my-skill\nversion: \"1\"\npublisher: me\ndescription: d\ncategory: note\ncontent:\n  abstract: a\n  note: b\n",
        )
        .unwrap();
        let now = chrono::Utc::now();
        let traces = home.join("traces");
        std::fs::create_dir_all(&traces).unwrap();
        std::fs::write(
            traces
                .join(now.format("%Y-%m-%d").to_string())
                .with_extension("jsonl"),
            format!(
                "{{\"ts\":\"{}\",\"method\":\"mur.skill.executed\",\"mur.skill.name\":\"my-skill\",\"mur.skill.outcome\":\"success\"}}\n",
                now.to_rfc3339()
            ),
        )
        .unwrap();

        let ctx = RepairCtx {
            home,
            registry_url: "unused",
        };
        let repair = StatsSidecarRepair;
        let finding = sidecar_finding("my-skill");
        assert!(repair.applicable(&finding));

        // Dry-run writes nothing.
        assert!(matches!(
            repair.run(&finding, &ctx, false),
            RepairOutcome::DryRun(_)
        ));
        assert!(
            SkillStats::load(&SkillStats::path(home, "my-skill"))
                .unwrap()
                .is_none()
        );

        // Apply rebuilds the sidecar from the trace.
        assert!(matches!(
            repair.run(&finding, &ctx, true),
            RepairOutcome::Fixed
        ));
        let stats = SkillStats::load(&SkillStats::path(home, "my-skill"))
            .unwrap()
            .unwrap();
        assert_eq!(stats.usage_count, 1);
        assert_eq!(stats.success_count, 1);
    }
}
