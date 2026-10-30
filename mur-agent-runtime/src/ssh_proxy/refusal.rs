//! Refusal reasons (plan Task 2.4). One enum, one sink.
//!
//! Every refusal is a bare `SSH_AGENT_FAILURE` to the ssh client, so the sink
//! is where the reason becomes visible: the log, the agent's channel, and the
//! refusals file `mur agent status` reads.

use std::path::PathBuf;

use mur_common::ssh_proxy_refusals::{self, SshRefusal};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalCode {
    PolicyInvalid,
    UpstreamUnavailable,
    NoSessionBind,
    SessionBindUnverified,
    KeyNotAllowed,
    HostNotAllowed,
    HitlUnavailable,
    /// Phase 2 only: every signature is refused until HITL is wired.
    HitlNotWired,
    PolicyUnpinned,
    MalformedRequest,
    /// The connecting process is not this agent or one it spawned.
    PeerNotSelf,
}

impl RefusalCode {
    pub const ALL: [Self; 11] = [
        Self::PolicyInvalid,
        Self::UpstreamUnavailable,
        Self::NoSessionBind,
        Self::SessionBindUnverified,
        Self::KeyNotAllowed,
        Self::HostNotAllowed,
        Self::HitlUnavailable,
        Self::HitlNotWired,
        Self::PolicyUnpinned,
        Self::MalformedRequest,
        Self::PeerNotSelf,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::PolicyInvalid => "policy_invalid",
            Self::UpstreamUnavailable => "upstream_unavailable",
            Self::NoSessionBind => "no_session_bind",
            Self::SessionBindUnverified => "session_bind_unverified",
            Self::KeyNotAllowed => "key_not_allowed",
            Self::HostNotAllowed => "host_not_allowed",
            Self::HitlUnavailable => "hitl_unavailable",
            Self::HitlNotWired => "hitl_not_wired",
            Self::PolicyUnpinned => "policy_unpinned",
            Self::MalformedRequest => "malformed_request",
            Self::PeerNotSelf => "peer_not_self",
        }
    }
}

/// One refusal, with whatever identities were known when it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: RefusalCode,
    pub key_fp: Option<String>,
    pub key_label: Option<String>,
    pub host_key_fp: Option<String>,
    /// Extra detail for the log (e.g. which algorithm failed to verify).
    pub detail: Option<String>,
}

impl Refusal {
    pub fn new(code: RefusalCode) -> Self {
        Self {
            code,
            key_fp: None,
            key_label: None,
            host_key_fp: None,
            detail: None,
        }
    }

    pub fn key(mut self, fp: impl Into<String>, label: Option<String>) -> Self {
        self.key_fp = Some(fp.into());
        self.key_label = label;
        self
    }

    pub fn host(mut self, fp: impl Into<String>) -> Self {
        self.host_key_fp = Some(fp.into());
        self
    }

    pub fn detail(mut self, d: impl Into<String>) -> Self {
        self.detail = Some(d.into());
        self
    }

    /// The text a person reads. `no_session_bind` uses the spec's exact words.
    pub fn message(&self) -> String {
        let key = match (&self.key_label, &self.key_fp) {
            (Some(l), Some(fp)) => format!("key {l} ({fp})"),
            (None, Some(fp)) => format!("key {fp}"),
            _ => "a request".to_string(),
        };
        let head = format!("ssh-agent proxy refused {key}");
        let sig_head = match &self.key_fp {
            Some(_) => format!("ssh-agent proxy refused a signature for {key}"),
            None => head.clone(),
        };
        let host = self.host_key_fp.as_deref().unwrap_or("unknown");
        let why = match self.code {
            RefusalCode::NoSessionBind => {
                return format!(
                    "{sig_head}:\nthe client did not identify the destination host \
                     (no session-bind@openssh.com).\nThis is not a key or policy problem. \
                     MUR needs the destination host to enforce\nthe policy, and this ssh \
                     client does not send it. Use OpenSSH 8.9 or newer."
                );
            }
            RefusalCode::PolicyInvalid => {
                "the entitlements.ssh policy is invalid, so every request is refused".into()
            }
            RefusalCode::UpstreamUnavailable => {
                "the upstream ssh-agent (SSH_AUTH_SOCK) is unreachable".into()
            }
            RefusalCode::SessionBindUnverified => {
                "the destination host key's session-bind signature did not verify".into()
            }
            RefusalCode::KeyNotAllowed => "this key is not listed in entitlements.ssh".into(),
            RefusalCode::HostNotAllowed => {
                format!("host key {host} is not allowed for this key in entitlements.ssh")
            }
            RefusalCode::HitlUnavailable => {
                "the approval channel is unavailable, and signatures are never passed through"
                    .into()
            }
            RefusalCode::HitlNotWired => format!(
                "policy allows host key {host}, but approval prompts are not wired yet, \
                 so this MUR version signs nothing"
            ),
            RefusalCode::PolicyUnpinned => {
                "this agent's entitlements have no trusted pin, so its ssh policy cannot be trusted"
                    .into()
            }
            RefusalCode::MalformedRequest => "the request was malformed or unsupported".into(),
            RefusalCode::PeerNotSelf => {
                "the connecting process is not this agent or a process it spawned, \
                 so the connection was closed before anything was forwarded"
                    .into()
            }
        };
        let detail = self
            .detail
            .as_deref()
            .map(|d| format!(" ({d})"))
            .unwrap_or_default();
        format!("{sig_head}: {why}{detail} [{}]", self.code.as_str())
    }
}

/// Where refusals go. Log always; the refusals file when there is a home;
/// the channel through a callback the supervisor provides, so this module
/// does not depend on the signing identity.
/// Posts one refusal (and its rendered message) to the agent's channel.
type ChannelPost = Box<dyn Fn(&Refusal, &str) + Send + Sync>;

pub struct RefusalSink {
    agent_home: Option<PathBuf>,
    channel: Option<ChannelPost>,
}

impl RefusalSink {
    pub fn new(agent_home: Option<PathBuf>) -> Self {
        Self {
            agent_home,
            channel: None,
        }
    }

    pub fn with_channel(mut self, f: impl Fn(&Refusal, &str) + Send + Sync + 'static) -> Self {
        self.channel = Some(Box::new(f));
        self
    }

    /// Record `r` everywhere. Best-effort: a failed write is logged, the
    /// request is refused regardless.
    pub fn record(&self, r: &Refusal) {
        let message = r.message();
        tracing::warn!(code = r.code.as_str(), "{message}");
        if let Some(home) = &self.agent_home {
            let entry = SshRefusal {
                code: r.code.as_str().to_string(),
                message: message.clone(),
                key_fp: r.key_fp.clone(),
                host_key_fp: r.host_key_fp.clone(),
                at_unix_ms: now_unix_ms(),
            };
            if let Err(e) = ssh_proxy_refusals::record(home, entry) {
                tracing::warn!(error = %e, "could not record the ssh-agent proxy refusal");
            }
        }
        if let Some(f) = &self.channel {
            f(r, &message);
        }
    }
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
