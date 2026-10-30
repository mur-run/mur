//! The `ssh_proxy:` line of `mur agent status`: what the runtime concluded at
//! its last start about whether the ssh-agent proxy is a boundary for this
//! agent. Read from `<agent_home>/ssh_proxy.json`, which the runtime writes.
//! Below it, the most recent refusals from `<agent_home>/ssh_proxy_refusals.json`
//! — the only place a refusal's reason is visible, since the ssh client just
//! sees `SSH_AGENT_FAILURE`.

use std::path::Path;

use mur_common::ssh_proxy_refusals::{self, SshRefusal};
use mur_common::ssh_proxy_status::{self, SshProxyStatus};

/// The line for the agent at `agent_home`, or `None` when the runtime has not
/// recorded one (an agent last started by an older runtime).
pub(super) fn line_for(agent_home: &Path) -> Option<String> {
    ssh_proxy_status::read(agent_home).map(|s| render(&s))
}

pub(super) fn render(s: &SshProxyStatus) -> String {
    format!("    ssh_proxy: {} — {}", s.label(), s.explanation)
}

/// Recent refusals, most recent first; empty when none were recorded.
pub(super) fn refusal_lines_for(agent_home: &Path) -> Vec<String> {
    render_refusals(&ssh_proxy_refusals::read(agent_home))
}

pub(super) fn render_refusals(rs: &[SshRefusal]) -> Vec<String> {
    if rs.is_empty() {
        return Vec::new();
    }
    let mut out = vec![format!("    ssh_proxy refusals (last {}):", rs.len())];
    out.extend(
        rs.iter()
            .map(|r| format!("      {} [{}] {}", when(r.at_unix_ms), r.code, r.message)),
    );
    out
}

fn when(at_unix_ms: u64) -> String {
    i64::try_from(at_unix_ms)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| at_unix_ms.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::ssh_proxy_status::SshProxyState;

    fn status(state: SshProxyState, code: Option<&str>) -> SshProxyStatus {
        SshProxyStatus {
            state,
            code: code.map(str::to_string),
            explanation: "why".into(),
            at_unix_ms: 1,
        }
    }

    #[test]
    fn renders_each_value() {
        assert_eq!(
            render(&status(SshProxyState::Off, None)),
            "    ssh_proxy: off — why"
        );
        assert_eq!(
            render(&status(SshProxyState::Boundary, None)),
            "    ssh_proxy: boundary — why"
        );
        assert_eq!(
            render(&status(SshProxyState::AuditOnly, Some("probe_reachable"))),
            "    ssh_proxy: audit-only (probe_reachable) — why"
        );
    }

    #[test]
    fn nothing_recorded_prints_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(line_for(tmp.path()).is_none());
        assert!(refusal_lines_for(tmp.path()).is_empty());
    }

    #[test]
    fn renders_recorded_refusals_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        for (i, code) in ["key_not_allowed", "hitl_denied"].iter().enumerate() {
            ssh_proxy_refusals::record(
                tmp.path(),
                SshRefusal {
                    code: (*code).into(),
                    message: format!("msg {i}"),
                    key_fp: None,
                    host_key_fp: None,
                    at_unix_ms: i as u64,
                },
            )
            .unwrap();
        }
        let lines = refusal_lines_for(tmp.path());
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "    ssh_proxy refusals (last 2):");
        assert!(lines[1].ends_with("[hitl_denied] msg 1"), "{}", lines[1]);
        assert!(
            lines[2].ends_with("[key_not_allowed] msg 0"),
            "{}",
            lines[2]
        );
    }
}
