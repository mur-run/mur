//! Repair impl for `dependency-freshness` findings (M5b).
//!
//! When a dependency is out of date, use the resolver to find the best
//! satisfying version and reinstall through `cmd_install` (preserving
//! DSSE verification).

use crate::cmd::skill_doctor::Finding;
use crate::skill_repair::{Repair, RepairCtx, RepairOutcome};

pub struct DepFreshnessRepair;

impl Repair for DepFreshnessRepair {
    fn check_id(&self) -> &'static str {
        "dependency-freshness"
    }

    fn applicable(&self, finding: &Finding) -> bool {
        finding.check_id == "dependency-freshness" && finding.fixable
    }

    fn run(&self, finding: &Finding, ctx: &RepairCtx, apply: bool) -> RepairOutcome {
        // Finding message format: "Required skill 'base' is not installed."
        // or "Requires base ^1.2.0 but 1.0.0 is installed."
        let msg = &finding.message;

        // Extract dependency name from message
        if msg.contains("is not installed") {
            // "Required skill '<name>' is not installed."
            let name = extract_quoted_name(msg).unwrap_or_default();
            if name.is_empty() {
                return RepairOutcome::Skipped("could not parse dependency name".into());
            }
            if apply {
                match crate::cmd::skill_install::cmd_install(ctx.home, ctx.registry_url, &name) {
                    Ok(()) => RepairOutcome::Fixed,
                    Err(e) => RepairOutcome::Failed(e),
                }
            } else {
                RepairOutcome::DryRun(format!("would install {name}"))
            }
        } else if msg.contains("but") && msg.contains("is installed") {
            // "Requires <name> <constraint> but <installed> is installed."
            let name = msg
                .split_whitespace()
                .nth(1)
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                return RepairOutcome::Skipped("could not parse dependency name".into());
            }
            if apply {
                match crate::cmd::skill_install::cmd_install(
                    ctx.home,
                    ctx.registry_url,
                    &name,
                ) {
                    Ok(()) => RepairOutcome::Fixed,
                    Err(e) => RepairOutcome::Failed(e),
                }
            } else {
                RepairOutcome::DryRun(format!("would reinstall {name}"))
            }
        } else {
            RepairOutcome::Skipped("unrecognised finding format".into())
        }
    }
}

fn extract_quoted_name(s: &str) -> Option<String> {
    let start = s.find('\'')?;
    let end = s[start + 1..].find('\'')?;
    Some(s[start + 1..start + 1 + end].to_string())
}
