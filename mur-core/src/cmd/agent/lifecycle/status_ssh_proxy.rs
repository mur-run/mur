//! The `ssh_proxy:` line of `mur agent status`: what the runtime concluded at
//! its last start about whether the ssh-agent proxy is a boundary for this
//! agent. Read from `<agent_home>/ssh_proxy.json`, which the runtime writes.

use std::path::Path;

use mur_common::ssh_proxy_status::{self, SshProxyState, SshProxyStatus};

/// The line for the agent at `agent_home`, or `None` when the runtime has not
/// recorded one (an agent last started by an older runtime).
pub(super) fn line_for(agent_home: &Path) -> Option<String> {
    ssh_proxy_status::read(agent_home).map(|s| render(&s))
}

pub(super) fn render(s: &SshProxyStatus) -> String {
    let value = match s.state {
        SshProxyState::Off => "off".to_string(),
        SshProxyState::Boundary => "boundary".to_string(),
        SshProxyState::AuditOnly => {
            format!("audit-only ({})", s.code.as_deref().unwrap_or("unknown"))
        }
    };
    format!("    ssh_proxy: {value} — {}", s.explanation)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
