//! Hub's SSH setup (SSH setup spec, "Hub"; slice 5). Every command calls the
//! one implementation in `mur_core::cmd::agent::ssh_setup`; no SSH rule lives
//! here or in TypeScript. A scanned plan stays in this process under an
//! opaque id (deviation H4), so Approve grants exactly what was scanned.

mod view;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use mur_agent_runtime::tools::ssh_request::SSH_REQUEST;
use mur_core::a2a_dial::{StepEvent, canonicalize_agent_name};
use mur_core::cmd::agent::ssh_setup::apply::{Lifecycle, Restart};
use mur_core::cmd::agent::ssh_setup::choices::{Choices, build_approval};
use mur_core::cmd::agent::ssh_setup::life::AgentLifecycle;
use mur_core::cmd::agent::ssh_setup::plan::split_keys;
use mur_core::cmd::agent::ssh_setup::probe::System;
use mur_core::cmd::agent::ssh_setup::{SetupPlan, front, pending, report};
use mur_core::cmd::agent::{SshRevokeArgs, cmd_ssh_revoke};
use tauri::State;

pub use view::{ApplyView, CheckView, OverviewView, PendingView, PlanView, RevokeView};

/// Emitted when an agent's `ssh_request` tool call completes in a Hub chat
/// (deviation H5). Payload: the agent name.
pub const SSH_REQUEST_EVENT: &str = "ssh-request-written";

/// How long a scanned plan stays usable before the card must rescan.
pub(crate) const PLAN_TTL: Duration = Duration::from_secs(10 * 60);
/// Open cards held at once; the oldest goes first.
pub(crate) const MAX_PLANS: usize = 16;

struct HeldPlan {
    plan: SetupPlan,
    reason: Option<String>,
    hand_back: String,
    at: Instant,
}

#[derive(Default)]
struct Held {
    plans: HashMap<String, HeldPlan>,
    /// Agents with an apply in flight: one stop / write / start at a time.
    applying: HashSet<String>,
}

#[derive(Default)]
pub struct SshPlans(Mutex<Held>);

/// An agent's apply slot; dropping it frees the agent for the next apply.
pub(crate) struct Applying<'a> {
    plans: &'a SshPlans,
    agent: String,
}

impl Drop for Applying<'_> {
    fn drop(&mut self) {
        self.plans.held().applying.remove(&self.agent);
    }
}

fn expired_text() -> String {
    format!(
        "this card is older than {} minutes or was already used; open it again to rescan \
         the host",
        PLAN_TTL.as_secs() / 60
    )
}

impl SshPlans {
    fn held(&self) -> std::sync::MutexGuard<'_, Held> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn put(&self, plan: SetupPlan, reason: Option<String>, hand_back: String) -> String {
        let id = uuid::Uuid::now_v7().to_string();
        let mut g = self.held();
        let m = &mut g.plans;
        m.retain(|_, h| h.at.elapsed() < PLAN_TTL);
        while m.len() >= MAX_PLANS {
            let Some(oldest) = m.iter().min_by_key(|(_, h)| h.at).map(|(k, _)| k.clone()) else {
                break;
            };
            m.remove(&oldest);
        }
        m.insert(
            id.clone(),
            HeldPlan {
                plan,
                reason,
                hand_back,
                at: Instant::now(),
            },
        );
        id
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.held().plans.len()
    }

    fn with<T>(&self, id: &str, f: impl FnOnce(&mut HeldPlan) -> T) -> Result<T, String> {
        let mut g = self.held();
        match g.plans.get_mut(id).filter(|h| h.at.elapsed() < PLAN_TTL) {
            Some(h) => Ok(f(h)),
            None => Err(expired_text()),
        }
    }

    /// Take the plan out, so a second Approve on the same card finds nothing.
    fn take(&self, id: &str) -> Result<HeldPlan, String> {
        let mut g = self.held();
        match g.plans.remove(id) {
            Some(h) if h.at.elapsed() < PLAN_TTL => Ok(h),
            _ => Err(expired_text()),
        }
    }

    /// Hand a taken plan back when nothing was written. It keeps its scan
    /// time, so a put-back never extends the TTL.
    fn put_back(&self, id: String, h: HeldPlan) {
        self.held().plans.insert(id, h);
    }

    /// Reserve `agent` for one apply; refused while another card for the
    /// same agent is applying.
    pub(crate) fn claim(&self, agent: &str) -> Result<Applying<'_>, String> {
        if !self.held().applying.insert(agent.to_string()) {
            return Err(format!(
                "an SSH approval for {agent} is already being applied; wait for it to finish"
            ));
        }
        Ok(Applying {
            plans: self,
            agent: agent.to_string(),
        })
    }
}

fn home() -> PathBuf {
    crate::mur_home_path()
}

fn canonical(name: &str) -> String {
    canonicalize_agent_name(&home(), name)
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("task failed: {e}"))?
}

/// Track `ssh_request` steps of one streamed turn; true when one finishes.
pub fn is_ssh_request_done(seen: &mut HashSet<String>, step: &StepEvent) -> bool {
    match step {
        StepEvent::Started { name, step_id, .. } if name == SSH_REQUEST => {
            seen.insert(step_id.clone());
            false
        }
        StepEvent::Completed { step_id, .. } => seen.remove(step_id),
        _ => false,
    }
}

/// What the agent's open requests look like now: one entry per request,
/// `host:port` and when it was asked. A one-shot dial (agent not running)
/// streams no steps, so chat compares this before and after the turn.
pub fn pending_stamp(mur_home: &std::path::Path, agent: &str) -> Vec<(String, i64)> {
    let mut v: Vec<(String, i64)> = pending::open_requests(mur_home, agent)
        .into_iter()
        .map(|o| {
            (
                o.request.target.canonical(),
                o.request.requested_at.timestamp(),
            )
        })
        .collect();
    v.sort();
    v
}

#[tauri::command]
pub async fn agent_ssh_overview(name: String) -> Result<OverviewView, String> {
    let name = canonical(&name);
    blocking(move || {
        let ov = front::overview(&System, &home(), &name)?;
        Ok(view::overview_view(&name, &ov))
    })
    .await
}

/// Scan the host once and hold the plan. A pending request for the same
/// `host:port` supplies the reason, the default login user and the hand-back.
#[tauri::command]
pub async fn agent_ssh_plan(
    plans: State<'_, SshPlans>,
    name: String,
    host: String,
    port: Option<u16>,
    user: Option<String>,
) -> Result<PlanView, String> {
    let name = canonical(&name);
    let (plan, reason, hand_back) = blocking(move || {
        let target = front::parse_target(host.trim(), port)?;
        let req = pending::open_requests(&home(), &name)
            .into_iter()
            .map(|o| o.request)
            .find(|r| r.target == target);
        let user = user
            .filter(|u| !u.trim().is_empty())
            .or_else(|| req.as_ref().and_then(|r| r.user.clone()));
        let hand_back = match &req {
            Some(r) => pending::hand_back_text(r),
            None => pending::direct_hand_back_text(&target),
        };
        let reason = req.map(|r| r.reason);
        let plan = front::plan_for(&home(), &name, target, user)?;
        Ok((plan, reason, hand_back))
    })
    .await?;
    let v_plan = plan.clone();
    let v_reason = reason.clone();
    let id = plans.put(plan, reason, hand_back);
    Ok(view::plan_view(&id, &v_plan, v_reason.as_deref()))
}

/// Re-read the ssh-agent after the user ran `ssh-add` (deviation H1).
#[tauri::command]
pub async fn agent_ssh_refresh_keys(
    plans: State<'_, SshPlans>,
    plan_id: String,
) -> Result<PlanView, String> {
    let (loaded, unloaded) = blocking(|| Ok(split_keys(&System))).await?;
    plans.with(&plan_id, |h| {
        h.plan.keys_loaded = loaded;
        h.plan.keys_unloaded = unloaded;
        view::plan_view(&plan_id, &h.plan, h.reason.as_deref())
    })
}

#[tauri::command]
pub fn agent_ssh_check(
    plans: State<'_, SshPlans>,
    plan_id: String,
    choices: Choices,
) -> Result<CheckView, String> {
    plans.with(&plan_id, |h| {
        view::check_view(build_approval(&h.plan, &choices))
    })
}

/// One profile write and one restart (S:§Journey 4). The plan is taken out
/// before anything runs, so it is used once; it goes back only when nothing
/// was written (not ready, agent busy, apply refused) and the card can retry.
#[tauri::command]
pub async fn agent_ssh_apply(
    plans: State<'_, SshPlans>,
    jobs: State<'_, crate::override_restart::jobs::OverrideJobs>,
    plan_id: String,
    choices: Choices,
) -> Result<ApplyView, String> {
    let held = plans.take(&plan_id)?;
    let agent = held.plan.agent.clone();
    let ready = build_approval(&held.plan, &choices)
        .map_err(|e| view::not_ready_text(&e))
        .and_then(|a| jobs.refuse_if_busy(&agent).map(|()| a))
        .and_then(|a| plans.claim(&agent).map(|slot| (a, slot)))
        .and_then(|(a, slot)| crate::fleet_pause::mur_binary().map(|exe| (a, slot, exe)));
    let (approval, _slot, exe) = match ready {
        Ok(r) => r,
        Err(e) => {
            plans.put_back(plan_id, held);
            return Err(e);
        }
    };
    let run_agent = agent.clone();
    let applied =
        match blocking(move || front::apply_for(&home(), &run_agent, &approval, exe)).await {
            Ok(a) => a,
            Err(e) => {
                // `apply_and_restart` writes in one locked, rolled-back
                // step and reports restart trouble in `Applied`, so an
                // error here means the profile is as it was.
                plans.put_back(plan_id, held);
                return Err(e);
            }
        };
    let hand_back = held.hand_back;
    let restarted = applied.restart == Restart::Restarted;
    Ok(ApplyView {
        lines: report::lines(&agent, &applied),
        restarted,
        can_restart: matches!(
            applied.restart,
            Restart::StopFailed(_) | Restart::StartFailed(_)
        ),
        hand_back: restarted.then_some(hand_back),
    })
}

/// Remove one pair, or a key with all its hosts. The UI confirms first, so
/// this passes the CLI's `--yes`. The CLI's "restart required" goes to a
/// stderr nobody sees here, so the view carries it as `restart_needed`.
#[tauri::command]
pub async fn agent_ssh_revoke(
    name: String,
    key_fp: String,
    host_key_fp: Option<String>,
) -> Result<RevokeView, String> {
    let name = canonical(&name);
    blocking(move || {
        cmd_ssh_revoke(&SshRevokeArgs {
            agent: &name,
            key: Some(&key_fp),
            host_key: host_key_fp.as_deref(),
            yes: true,
        })
        .map_err(|e| format!("{e:#}"))?;
        let restart_needed = AgentLifecycle::new(&name, &home()).running_pid().is_some();
        let ov = front::overview(&System, &home(), &name)?;
        Ok(RevokeView {
            overview: view::overview_view(&name, &ov),
            restart_needed,
        })
    })
    .await
}

/// S:§Unattended requests: "Each listed request has a Dismiss action".
#[tauri::command]
pub async fn agent_ssh_dismiss(name: String, host: String, port: u16) -> Result<(), String> {
    let name = canonical(&name);
    blocking(move || {
        let target = front::parse_target(&host, Some(port))?;
        pending::dismiss(&home(), &name, &target)
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
    .await
}

/// Every agent's open requests, for "Needs you". No scan, no network.
#[tauri::command]
pub async fn ssh_pending_all() -> Result<Vec<PendingView>, String> {
    blocking(|| {
        Ok(pending::open_everywhere(&home())
            .iter()
            .map(|(agent, o)| view::pending_view(agent, o))
            .collect())
    })
    .await
}
