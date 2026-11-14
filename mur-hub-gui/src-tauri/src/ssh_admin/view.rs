//! What the WebView sees. Names and display data only: every SSH rule ran
//! in `mur_core::cmd::agent::ssh_setup` before a view is built.

use mur_core::cmd::agent::ssh_setup::choices::NotReady;
use mur_core::cmd::agent::ssh_setup::front::Overview;
use mur_core::cmd::agent::ssh_setup::pending::OpenRequest;
use mur_core::cmd::agent::ssh_setup::probe::{AgentKey, private_key_of};
use mur_core::cmd::agent::ssh_setup::{Approval, SetupPlan, SpawnChange, Verdict};
use mur_core::cmd::agent::ssh_status::PairState;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct KeyView {
    pub fingerprint: String,
    pub comment: String,
    pub loaded: bool,
    /// For an unloaded key: the command that loads it (deviation H1).
    pub add_cmd: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HostKeyView {
    pub fingerprint: String,
    pub key_type: String,
    pub matched: bool,
}

#[derive(Debug, Serialize)]
pub struct PlanView {
    pub plan_id: String,
    pub agent: String,
    pub host: String,
    pub user: Option<String>,
    /// The agent's own words, already sanitized by the request reader.
    pub reason: Option<String>,
    pub keys: Vec<KeyView>,
    pub scanned: Vec<HostKeyView>,
    pub verdict: &'static str,
    pub ticks_offered: bool,
    pub spawn: Option<&'static str>,
    pub consent_text: Option<String>,
    pub differs_cmd: Option<String>,
}

/// Quote `s` for a POSIX shell unless it is plainly safe.
pub fn sh_quote(s: &str) -> String {
    let plain = !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-~+@".contains(&b));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn spawn_name(s: SpawnChange) -> &'static str {
    match s {
        SpawnChange::AddSsh => "add_ssh",
        SpawnChange::NoneToStrict => "none_to_strict",
    }
}

fn verdict_name(v: &Verdict) -> &'static str {
    match v {
        Verdict::Trusted => "trusted",
        Verdict::Unknown { downgraded: false } => "unknown",
        Verdict::Unknown { downgraded: true } => "unknown_downgraded",
        Verdict::Differs => "differs",
    }
}

fn key_view(k: &AgentKey, loaded: bool) -> KeyView {
    KeyView {
        fingerprint: k.fingerprint.clone(),
        comment: k.comment.clone(),
        loaded,
        add_cmd: (!loaded).then_some(k.path.as_deref()).flatten().map(|p| {
            format!(
                "ssh-add {}",
                sh_quote(&private_key_of(p).display().to_string())
            )
        }),
    }
}

pub fn plan_view(id: &str, p: &SetupPlan, reason: Option<&str>) -> PlanView {
    let keys = p
        .keys_loaded
        .iter()
        .map(|k| key_view(k, true))
        .chain(p.keys_unloaded.iter().map(|k| key_view(k, false)))
        .collect();
    PlanView {
        plan_id: id.to_string(),
        agent: p.agent.clone(),
        host: p.host.canonical(),
        user: p.user.clone(),
        reason: reason.map(str::to_string),
        keys,
        scanned: p
            .scanned
            .iter()
            .map(|k| HostKeyView {
                fingerprint: k.fingerprint.clone(),
                key_type: k.entry.key_type.clone(),
                matched: p.matched.contains(&k.fingerprint),
            })
            .collect(),
        verdict: verdict_name(&p.verdict),
        ticks_offered: matches!(p.verdict, Verdict::Unknown { .. }),
        spawn: p.spawn.map(spawn_name),
        consent_text: (p.spawn == Some(SpawnChange::NoneToStrict))
            .then(|| p.exec_surface.as_ref().map(|e| e.consent_text()))
            .flatten(),
        differs_cmd: (p.verdict == Verdict::Differs)
            .then(|| format!("ssh-keygen -R {}", p.host.host())),
    }
}

#[derive(Debug, Serialize)]
pub struct ReadyView {
    pub key_fp: String,
    pub host_keys: Vec<String>,
    pub spawn: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct CheckView {
    pub ready: Option<ReadyView>,
    pub not_ready: Option<NotReady>,
}

pub fn check_view(r: Result<Approval, NotReady>) -> CheckView {
    match r {
        Ok(a) => CheckView {
            ready: Some(ReadyView {
                key_fp: a.key_fp,
                host_keys: a.hosts.into_iter().map(|h| h.host_key_fp).collect(),
                spawn: a.spawn.map(spawn_name),
            }),
            not_ready: None,
        },
        Err(e) => CheckView {
            ready: None,
            not_ready: Some(e),
        },
    }
}

#[derive(Debug, Serialize)]
pub struct ApplyView {
    pub lines: Vec<String>,
    pub restarted: bool,
    /// Written, but the runtime did not come back: offer Restart.
    pub can_restart: bool,
    /// Sent to the agent by the UI only when `restarted`.
    pub hand_back: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HostRuleView {
    pub host_key: String,
    pub label: Option<String>,
    pub names: Vec<String>,
    pub state: &'static str,
    pub signed_at: Option<String>,
    pub last_expired_mark: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct KeyRuleView {
    pub fingerprint: String,
    pub label: Option<String>,
    pub loaded: bool,
    pub hosts: Vec<HostRuleView>,
}

#[derive(Debug, Serialize)]
pub struct PendingView {
    pub agent: String,
    pub host: String,
    pub port: u16,
    pub canonical: String,
    pub user: Option<String>,
    pub reason: String,
    /// Unix seconds.
    pub requested_at: i64,
    pub granted: bool,
}

#[derive(Debug, Serialize)]
pub struct OverviewView {
    pub keys: Vec<KeyRuleView>,
    pub pending: Vec<PendingView>,
    pub decisions_unverified: Option<String>,
}

pub fn pending_view(agent: &str, o: &OpenRequest) -> PendingView {
    let r = &o.request;
    PendingView {
        agent: agent.to_string(),
        host: r.target.host().to_string(),
        port: r.target.port(),
        canonical: r.target.canonical(),
        user: r.user.clone(),
        reason: r.reason.clone(),
        requested_at: r.requested_at.timestamp(),
        granted: o.granted,
    }
}

pub fn overview_view(agent: &str, ov: &Overview) -> OverviewView {
    let keys = ov
        .ssh
        .keys
        .iter()
        .map(|k| KeyRuleView {
            fingerprint: k.fingerprint.clone(),
            label: k.label.clone(),
            loaded: ov.loaded.contains(&k.fingerprint),
            hosts: k
                .allow_hosts
                .iter()
                .map(|h| {
                    let pair = ov
                        .status
                        .pairs
                        .iter()
                        .find(|p| p.key_fp == k.fingerprint && p.host_key_fp == h.host_key);
                    let (state, signed_at) = match pair.map(|p| &p.state) {
                        Some(PairState::Signed { at }) => ("signed", Some(at.to_rfc3339())),
                        Some(PairState::Approved) => ("approved", None),
                        _ => ("not_approved", None),
                    };
                    let names = pair
                        .map(|p| p.host_names.clone())
                        .filter(|n| !n.is_empty())
                        .unwrap_or_else(|| {
                            h.known_hosts.iter().map(|kh| kh.host.clone()).collect()
                        });
                    HostRuleView {
                        host_key: h.host_key.clone(),
                        label: h.label.clone(),
                        names,
                        state,
                        signed_at,
                        last_expired_mark: pair
                            .and_then(|p| p.last_expired_mark)
                            .map(|t| t.to_rfc3339()),
                    }
                })
                .collect(),
        })
        .collect();
    OverviewView {
        keys,
        pending: ov.open.iter().map(|o| pending_view(agent, o)).collect(),
        decisions_unverified: ov.status.decisions_unverified.clone(),
    }
}
