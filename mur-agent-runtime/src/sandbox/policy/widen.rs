use super::*;

impl SandboxPolicy {
    /// Grant outbound to `extra` TCP ports for the agent's own LLM endpoint.
    /// Under Restricted (non-empty general list) they join the general list
    /// (`*:port`). Under ProxyOnly (general list present-but-empty AND
    /// `net_loopback_allowed`) the LLM is a loopback cc-proxy, so route its port
    /// to the loopback carve-out instead of opening a general `*:port`. No-op
    /// under Off (empty list, flag false) and Unrestricted (`None`).
    pub fn allow_extra_ports(&mut self, extra: &[u16]) {
        match self.net_allow_ports.as_ref().map(|p| p.is_empty()) {
            Some(false) => {
                if let Some(ports) = &mut self.net_allow_ports {
                    for p in extra {
                        if !ports.contains(p) {
                            ports.push(*p);
                        }
                    }
                }
            }
            Some(true) if self.net_loopback_allowed => {
                for p in extra {
                    if !self.net_allow_loopback_ports.contains(p) {
                        self.net_allow_loopback_ports.push(*p);
                    }
                }
            }
            _ => {}
        }
    }

    /// Grant loopback-only access to `extra` TCP ports (the egress proxy, and —
    /// under ProxyOnly — the LLM cc-proxy). Fires for Restricted (non-empty
    /// general list) and ProxyOnly (`net_loopback_allowed`); a no-op under Off
    /// (empty list, flag false) and Unrestricted (`None`).
    pub fn allow_loopback_ports(&mut self, extra: &[u16]) {
        let permitted =
            matches!(&self.net_allow_ports, Some(p) if !p.is_empty()) || self.net_loopback_allowed;
        if permitted {
            for p in extra {
                if !self.net_allow_loopback_ports.contains(p) {
                    self.net_allow_loopback_ports.push(*p);
                }
            }
        }
    }

    /// Reopen the Restricted general-port set on the RUNTIME's own (self)
    /// profile when it hosts the in-process egress proxy. Under ProxyOnly the
    /// worker's entitlements deny all general TCP — but the egress proxy runs
    /// inside the runtime process, so that deny also killed the proxy's
    /// UPSTREAM dials (`TcpStream::connect` → EPERM, os error 1) and every
    /// sandboxed child's granted egress died after `CONNECT ALLOW`. The child
    /// profiles are built separately (`sandbox::child::spawn_sandboxed`) and
    /// keep the strict ProxyOnly deny, so the choke point for untrusted MCP
    /// children is unchanged; the runtime's own LLM client remains
    /// HostGuard-gated exactly as under Restricted. No-op for Off (air-gapped
    /// stays air-gapped), Restricted, and Unrestricted.
    pub fn allow_in_process_proxy_upstream(&mut self) {
        if matches!(&self.net_allow_ports, Some(p) if p.is_empty()) && self.net_loopback_allowed {
            self.net_allow_ports = Some(RESTRICTED_GENERAL_PORTS.to_vec());
        }
    }

    /// Grant write access to additional paths the runtime owns but that live
    /// outside `agent_home` — e.g. the shared `~/.mur/runtime` media state
    /// (`watch.json`, VLC snapshot dir) that the co-watching scheduler must
    /// persist to and clean up. Idempotent.
    pub fn allow_extra_write_paths(&mut self, paths: &[PathBuf]) {
        for p in paths {
            if !self.fs_write.contains(p) {
                self.fs_write.push(p.clone());
            }
        }
    }
}
