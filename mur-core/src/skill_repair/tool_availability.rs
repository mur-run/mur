//! Repair impl for `tool-availability` findings (M5b).
//!
//! Currently always `Skipped` — the CLI doctor returns `Unknown` for
//! tool-availability (requires agent context). When called from within
//! an agent that has a trust capability list, the agent-level repair can
//! check mur-managed skills and reinstall missing MCP servers. This stub
//! is the safe default: no silent provisioning of arbitrary MCP servers.

use crate::cmd::skill_doctor::Finding;
use crate::skill_repair::{Repair, RepairCtx, RepairOutcome};

pub struct ToolAvailabilityRepair;

impl Repair for ToolAvailabilityRepair {
    fn check_id(&self) -> &'static str {
        "tool-availability"
    }

    fn applicable(&self, finding: &Finding) -> bool {
        finding.check_id == "tool-availability" && finding.fixable
    }

    fn run(&self, finding: &Finding, _ctx: &RepairCtx, _apply: bool) -> RepairOutcome {
        RepairOutcome::Skipped(format!("manual MCP install required: {}", finding.message))
    }
}
