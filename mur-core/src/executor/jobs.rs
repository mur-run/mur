//! Ephemeral parallel-jobs fan-out: build an in-memory DAG of rank-0
//! `delegate_to` steps (one per job) and run it through `execute_dag` — no
//! authored workflow/skill file. Generalizes fleet broadcast (`cmd/fleet/run.rs`)
//! to per-job prompts with a free assignee. See
//! `docs/superpowers/specs/2026-06-24-parallel-jobs-dynamic-fanout-design.md`.

use mur_common::skill::manifest::{Procedure, ProcedureStep};

/// A single job: a prompt and the (canonicalized) agent to delegate it to.
pub struct Job {
    pub description: String,
    pub assignee: String,
}

/// One rank-0 `ProcedureStep` per job (all parallel, no deps). Sets BOTH
/// `intent` (the delegate prompt) and `description` (channel/ledger labels)
/// to the job text, and a stable unique `id` for idempotency / crash-resume.
pub fn build_jobs_procedure(jobs: &[Job]) -> Procedure {
    Procedure {
        variables: vec![],
        steps: jobs
            .iter()
            .enumerate()
            .map(|(i, j)| ProcedureStep {
                description: j.description.clone(),
                intent: Some(j.description.clone()),
                delegate_to: Some(j.assignee.clone()),
                id: Some(format!("job-{i}")),
                ..Default::default()
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_jobs_procedure_one_rank0_step_per_job() {
        let jobs = vec![
            Job { description: "add caching to fetch".into(), assignee: "rustsmith".into() },
            Job { description: "write the README".into(), assignee: "frontend".into() },
        ];
        let p = build_jobs_procedure(&jobs);
        assert_eq!(p.steps.len(), 2);
        // delegate target per job
        assert_eq!(p.steps[0].delegate_to.as_deref(), Some("rustsmith"));
        assert_eq!(p.steps[1].delegate_to.as_deref(), Some("frontend"));
        // BOTH intent (prompt) and description (labels) carry the job text
        assert_eq!(p.steps[0].intent.as_deref(), Some("add caching to fetch"));
        assert_eq!(p.steps[0].description, "add caching to fetch");
        // stable, unique ids
        assert_eq!(p.steps[0].id.as_deref(), Some("job-0"));
        assert_eq!(p.steps[1].id.as_deref(), Some("job-1"));
        // all rank-0 (no dependencies => all parallel)
        assert!(p.steps.iter().all(|s| s.depends_on.is_empty()));
    }
}
