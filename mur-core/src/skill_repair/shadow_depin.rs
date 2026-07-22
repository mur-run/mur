//! Repair impl for `shadow-drift` findings: de-pin an agent-local vendored
//! copy that is byte-identical to the global builtin. Only identical shadows
//! are marked `fixable` (see `run_shadow_drift`), so this repair never touches
//! a diverged copy that might carry a real local edit.

use crate::cmd::skill_doctor::Finding;
use crate::skill_repair::{Repair, RepairCtx, RepairOutcome};

pub struct ShadowDepinRepair;

/// Recover `(agent, skill)` from the finding's own remediation string
/// (`mur agent skill remove <agent> <skill>`) — an internal contract with
/// `run_shadow_drift`, which generates exactly that form.
fn parse_agent_skill(remediation: &str) -> Option<(String, String)> {
    let rest = remediation.strip_prefix("mur agent skill remove ")?;
    let mut it = rest.split_whitespace();
    Some((it.next()?.to_string(), it.next()?.to_string()))
}

impl Repair for ShadowDepinRepair {
    fn check_id(&self) -> &'static str {
        "shadow-drift"
    }

    fn applicable(&self, finding: &Finding) -> bool {
        finding.check_id == "shadow-drift" && finding.fixable
    }

    fn run(&self, finding: &Finding, ctx: &RepairCtx, apply: bool) -> RepairOutcome {
        let Some((agent, skill)) = finding.remediation.as_deref().and_then(parse_agent_skill)
        else {
            return RepairOutcome::Skipped("no parseable remediation".into());
        };
        if !apply {
            return RepairOutcome::DryRun(format!(
                "would de-pin shadow '{skill}' from agent '{agent}'"
            ));
        }
        match crate::cmd::agent::skill::depin_skill(ctx.home, &agent, &skill) {
            Ok(true) => RepairOutcome::Fixed,
            Ok(false) => RepairOutcome::Skipped(format!("'{skill}' already absent on '{agent}'")),
            Err(e) => RepairOutcome::Failed(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::skill_doctor::{Finding, Severity};

    fn shadow_finding() -> Finding {
        Finding {
            check_id: "shadow-drift".into(),
            category: "shadow".into(),
            severity: Severity::Ok,
            skill_name: "mur-compress".into(),
            message: "redundant".into(),
            remediation: Some("mur agent skill remove mur mur-compress".into()),
            fixable: true,
        }
    }

    // Full valid profile, mirroring `CARD_ONLY_PROFILE` in
    // `cmd::agent::skill` tests: `mur-compress` is carried as an
    // `installed_skills` card + on-disk dir but NOT as a `skills:` ref (the
    // exact identical-shadow shape `run_shadow_drift` flags as fixable).
    const CARD_ONLY_PROFILE: &str = "\
schema: 1
id: 0192f5a1-28ab-7111-8000-000000000099
name: mur
display_name: MUR
version: \"0.1.0\"
persona:
  category: research
  description: test profile for shadow_depin tests
  traits: { tone: concise, risk: cautious, verbosity: low }
sys_prompt_file: \"sys_prompt.md\"
model: { provider: ollama, name: \"m\", params: {} }
mcp_servers: []
skills:
  - skills/concierge
installed_skills:
- name: mur-compress
  version: 1.0.0
  publisher: human:mur
  description: d
  category: context
  abstract: a
transport: { stdio: true, socket: { enabled: true, bind: \"unix:///tmp/a.sock\" } }
communication: { accepts_from: [\"*\"], sends_to: [] }
capabilities: [\"a2a.message.send\",\"a2a.tasks\"]
entitlements:
  network:
    inbound: { ports: [] }
    outbound: { mode: restricted, allow_hosts: [], protocols: [\"tcp\"], resolve_dns: { mode: system } }
  filesystem: { read: [], write: [], deny: [\"~/.ssh\"] }
  processes: { spawn: { mode: allowlist, allowed: [] } }
  syscalls: { mode: default }
  limits: { memory_mb: 512, file_descriptors: 1024, processes: 32 }
notifications: { on_task_complete: [], on_error: [], on_shutdown: [] }
retry:
  llm: { max_retries: 3, backoff: exponential, initial_delay_ms: 1000, max_delay_ms: 30000, retry_on: [\"rate_limit\"] }
  tool: { max_retries: 1, backoff: fixed, initial_delay_ms: 500 }
lifecycle: { restart: on_failure, max_restarts: 3, restart_window_secs: 600, stop_timeout_secs: 15, mcp_required: true }
created_at: \"2026-04-22T10:00:00+08:00\"
updated_at: \"2026-04-22T10:00:00+08:00\"
";

    fn seed_shadow(home: &std::path::Path) {
        let dir = home.join("agents/mur");
        std::fs::create_dir_all(dir.join("skills/mur-compress")).unwrap();
        std::fs::write(dir.join("profile.yaml"), CARD_ONLY_PROFILE).unwrap();
        std::fs::write(
            dir.join("skills/mur-compress/skill.yaml"),
            "name: mur-compress\nversion: 1.0.0\npublisher: human:mur\ndescription: d\ncategory: context\ncontent:\n  abstract: a\n  context: b\n",
        )
        .unwrap();
    }

    #[test]
    fn applicable_only_for_fixable_shadow_findings() {
        let repair = ShadowDepinRepair;
        assert!(repair.applicable(&shadow_finding()));
        let mut not_fixable = shadow_finding();
        not_fixable.fixable = false;
        assert!(!repair.applicable(&not_fixable));
    }

    #[test]
    fn apply_depins_the_shadow() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        seed_shadow(home);
        let ctx = RepairCtx {
            home,
            registry_url: "unused",
        };
        let repair = ShadowDepinRepair;
        let finding = shadow_finding();

        // dry-run touches nothing
        assert!(matches!(
            repair.run(&finding, &ctx, false),
            RepairOutcome::DryRun(_)
        ));
        assert!(home.join("agents/mur/skills/mur-compress").exists());

        // apply removes card + dir
        assert!(matches!(
            repair.run(&finding, &ctx, true),
            RepairOutcome::Fixed
        ));
        assert!(!home.join("agents/mur/skills/mur-compress").exists());
        let yaml = std::fs::read_to_string(home.join("agents/mur/profile.yaml")).unwrap();
        assert!(!yaml.contains("mur-compress"));
    }
}
