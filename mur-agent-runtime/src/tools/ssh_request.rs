//! Built-in `ssh_request` tool, and the SSH facts the prompt and the refusal
//! hints share (SSH setup journey, #2344).
//!
//! The one route an agent has to a new SSH host. It never grants anything: a
//! host already in `entitlements.ssh` gets the live state back, any other host
//! gets one request file under `<agent_home>/ssh-requests/` that murmur and Hub
//! turn into a card the user must still approve. Registered for every agent,
//! including one with no SSH grant — an agent that does not know the route
//! exists is the failure this exists to fix (K1).
//!
//! The tool cannot tell whether anyone is watching the turn, so it writes the
//! file on every call and its reply never claims the user saw it.

use std::path::PathBuf;

use mur_common::agent::{ProcessesEntitlement, SshEntitlement};
use mur_common::ssh_proxy_status::{self, SshProxyStatus};
use mur_ssh_request::{RequestHost, SshRequest};

use crate::tools::{ToolDef, ToolError, ToolExecutor, ToolOutput};

pub const SSH_REQUEST: &str = "ssh_request";

/// The SSH-relevant part of the profile, captured at boot. Entitlements are
/// cold fields (a change needs a restart), so a copy taken at boot is current
/// for the life of the process.
#[derive(Debug, Clone)]
pub struct SshFacts {
    pub ssh: SshEntitlement,
    /// Whether the spawn policy lets the agent exec `ssh`.
    pub spawnable: bool,
}

impl SshFacts {
    pub fn new(ssh: &SshEntitlement, processes: &ProcessesEntitlement) -> Self {
        Self {
            ssh: ssh.clone(),
            spawnable: mur_ssh_request::ssh_spawnable(processes),
        }
    }

    /// Every `(key, host key)` pair granted for `target`, rendered for the
    /// agent. A host matches on the `known_hosts` name the CLI confirmed it
    /// under (`name`, or `[name]:port` off port 22), case-insensitively like
    /// OpenSSH — the CLI stores the name as `ssh-keyscan` echoed it.
    /// `host_key` stays the only identity the proxy enforces; this is only how
    /// the agent is told.
    pub fn granted(&self, target: &RequestHost) -> Vec<String> {
        let name = target.known_hosts_name();
        let mut out = Vec::new();
        for key in &self.ssh.keys {
            for host in &key.allow_hosts {
                if host
                    .known_hosts
                    .iter()
                    .any(|kh| kh.host.eq_ignore_ascii_case(&name))
                {
                    out.push(format!(
                        "key {} → host key {}",
                        key_label(key.label.as_deref(), &key.fingerprint),
                        host.host_key
                    ));
                }
            }
        }
        out
    }

    /// One line per allowed host, for the prompt.
    fn allowed_hosts(&self) -> Vec<String> {
        let mut out = Vec::new();
        for key in &self.ssh.keys {
            let k = key_label(key.label.as_deref(), &key.fingerprint);
            for host in &key.allow_hosts {
                let names: Vec<&str> = host.known_hosts.iter().map(|kh| kh.host.as_str()).collect();
                let names = if names.is_empty() {
                    format!("host key {}", host.host_key)
                } else {
                    names.join(", ")
                };
                out.push(format!("{names} (key {k})"));
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

fn key_label(label: Option<&str>, fp: &str) -> String {
    match label {
        Some(l) => format!("{l} ({fp})"),
        None => fp.to_string(),
    }
}

fn proxy_line(proxy: Option<&SshProxyStatus>) -> String {
    match proxy {
        Some(p) => format!("ssh-agent proxy: {} — {}", p.label(), p.explanation),
        None => "ssh-agent proxy: not recorded".to_string(),
    }
}

fn exec_line(spawnable: bool) -> &'static str {
    if spawnable {
        "`ssh` can be exec'd: yes"
    } else {
        "`ssh` can be exec'd: no (the spawn policy does not allow it; approving a request can change that)"
    }
}

/// What the agent is told about the route, in the prompt and in every hint.
const ROUTE: &str = "To reach a host that is not listed, call `ssh_request` with the host, \
port, login user and a one-line reason, then stop and tell the user you asked. MUR shows the \
request to the user, who approves it there; you are then restarted with access and handed the \
request back. Do not ask the user to run commands (`mur agent ssh ...`, `ssh-keygen`, editing \
`~/.ssh`), and do not try to read or change SSH keys or `known_hosts` yourself.";

/// The SSH section of the system prompt, built once at boot.
pub fn prompt_section(facts: &SshFacts, proxy: Option<&SshProxyStatus>) -> String {
    let hosts = facts.allowed_hosts();
    let mut s = String::from("\n\n## SSH\n");
    if hosts.is_empty() {
        s.push_str("Allowed hosts: none.\n");
    } else {
        s.push_str("Allowed hosts:\n");
        for h in &hosts {
            s.push_str(&format!("- {h}\n"));
        }
        s.push_str(&format!("{}\n", proxy_line(proxy)));
    }
    s.push_str(&format!("{}\n", exec_line(facts.spawnable)));
    s.push_str(ROUTE);
    s
}

/// The hint for a denied exec of `ssh`. `None` for any other binary.
pub fn spawn_denied_hint(bin: &str) -> Option<String> {
    let name = std::path::Path::new(bin).file_name()?.to_str()?;
    (name == mur_ssh_request::SSH_BINARY).then(|| {
        format!(
            "\n\n[sandbox] `ssh` is not in this agent's spawn allowlist. Do not ask for it to be \
             granted by hand. {ROUTE}"
        )
    })
}

/// The hint for an ssh login the proxy refused (ssh prints `agent refused
/// operation` when the agent answers a sign request with failure).
pub fn proxy_refused_hint(stderr: &str) -> Option<String> {
    stderr.contains("agent refused operation").then(|| {
        format!(
            "\n\n[ssh] MUR's ssh-agent proxy refused this login: the key or host is not allowed \
             for this agent. {ROUTE}"
        )
    })
}

pub struct SshRequestTool {
    pub agent_home: PathBuf,
    pub facts: SshFacts,
}

impl SshRequestTool {
    fn allowed_reply(&self, target: &RequestHost, granted: &[String]) -> String {
        let proxy = ssh_proxy_status::read(&self.agent_home);
        format!(
            "{} is already allowed for this agent; nothing was requested.\n{}\n{}\n{}\n\
             Run one test connection before relying on it.",
            target.canonical(),
            granted.join("\n"),
            proxy_line(proxy.as_ref()),
            exec_line(self.facts.spawnable),
        )
    }
}

#[async_trait::async_trait]
impl ToolExecutor for SshRequestTool {
    fn name(&self) -> &str {
        SSH_REQUEST
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: SSH_REQUEST.into(),
            description: "Ask the user for SSH access to a host. The only route to a new SSH \
host: never tell the user to run commands or edit ~/.ssh. If the host is already allowed, this \
returns the live SSH state and requests nothing. Otherwise it records one request that MUR shows \
the user; stop after calling it and say you asked. Once approved you are restarted with access \
and handed the request back — run one test connection first."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "host": {"type": "string", "description": "DNS name or IP address"},
                    "port": {"type": "integer", "description": "Default 22"},
                    "user": {"type": "string", "description": "Login user, if known"},
                    "reason": {"type": "string", "description": "One line: why you need it"}
                },
                "required": ["host", "reason"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let host = input
            .get("host")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .ok_or_else(|| ToolError::InvalidInput("`host` is required".into()))?;
        let port = match input.get("port") {
            None | Some(serde_json::Value::Null) => None,
            Some(v) => Some(
                v.as_u64()
                    .and_then(|p| u16::try_from(p).ok())
                    .ok_or_else(|| ToolError::InvalidInput("`port` must be 1-65535".into()))?,
            ),
        };
        let target =
            RequestHost::parse(host, port).map_err(|e| ToolError::InvalidInput(e.to_string()))?;

        let granted = self.facts.granted(&target);
        if !granted.is_empty() {
            return Ok(self.allowed_reply(&target, &granted).into());
        }

        let user = input
            .get("user")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let reason = input.get("reason").and_then(|v| v.as_str()).unwrap_or("");
        let req = SshRequest::new(&target, user, reason, chrono::Utc::now())
            .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let written = mur_ssh_request::write(&self.agent_home, &req)
            .map_err(|e| ToolError::Execution(format!("record the ssh request: {e}")))?;
        Ok(format!(
            "Request recorded for {}{}. MUR lists it for the user in murmur and Hub; nothing is \
             granted until they approve it there. Stop now and tell the user you asked for SSH \
             access to this host and why. Do not ask them to run commands. After approval you are \
             restarted and handed the request back: run one test connection, and continue only \
             if it passes.",
            written.target.canonical(),
            if written.replaced {
                " (it replaces your earlier request for the same host)"
            } else {
                ""
            },
        )
        .into())
    }
}

#[cfg(test)]
#[path = "ssh_request_tests.rs"]
mod tests;
