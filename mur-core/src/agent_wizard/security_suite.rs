//! Optional AgentDojo/HarmBench security suites for high-risk agents.
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub enum SuiteOutcome {
    Skipped(String),
    Gated { passed: bool },
}

/// If a suite JSONL exists at `jsonl`, aggregate + gate it; else Skip with a reason.
pub fn evaluate_jsonl(jsonl: &Path) -> SuiteOutcome {
    if !jsonl.exists() {
        return SuiteOutcome::Skipped(format!(
            "no suite output at {} (run scripts/eval/*/run.py)",
            jsonl.display()
        ));
    }
    match crate::cmd::agent_eval::aggregate_jsonl(jsonl) {
        Ok(agg) => SuiteOutcome::Gated {
            passed: crate::cmd::agent_eval::all_gates_pass(&agg),
        },
        Err(e) => SuiteOutcome::Skipped(format!("unreadable suite output: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_jsonl_skips_gracefully() {
        match evaluate_jsonl(Path::new("/nonexistent/x.jsonl")) {
            SuiteOutcome::Skipped(_) => {}
            other => panic!("expected Skipped, got {other:?}"),
        }
    }
}
