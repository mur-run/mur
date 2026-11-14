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
use mur_core::cmd::agent::ssh_setup::apply::Restart;
use mur_core::cmd::agent::ssh_setup::choices::{Choices, build_approval};
use mur_core::cmd::agent::ssh_setup::plan::split_keys;
use mur_core::cmd::agent::ssh_setup::probe::System;
use mur_core::cmd::agent::ssh_setup::{SetupPlan, front, pending, report};
use mur_core::cmd::agent::{SshRevokeArgs, cmd_ssh_revoke};
use tauri::State;

pub use view::{ApplyView, CheckView, OverviewView, PendingView, PlanView};

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
pub struct SshPlans(Mutex<HashMap<String, HeldPlan>>);

impl SshPlans {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, HeldPlan>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn put(&self, plan: SetupPlan, reason: Option<String>, hand_back: String) -> String {
        let id = uuid::Uuid::now_v7().to_string();
        let mut m = self.lock();
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
        self.lock().len()
    }

    fn with<T>(&self, id: &str, f: impl FnOnce(&mut HeldPlan) -> T) -> Result<T, String> {
        let mut m = self.lock();
        match m.get_mut(id).filter(|h| h.at.elapsed() < PLAN_TTL) {
            Some(h) => Ok(f(h)),
            None => Err(format!(
                "this card is older than {} minutes or was already used; open it again to \
                 rescan the host",
                PLAN_TTL.as_secs() / 60
            )),
        }
    }

    fn remove(&self, id: &str) {
        self.lock().remove(id);
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

#[tauri::command]
pub async fn agent_ssh_overview(name: String) -> Result<OverviewView, String> {
    let name = canonical(&name);
    blocking(move || {
        let ov = front::overview(&System, &home(), &name).map_err(|e| format!("{e:#}"))?;
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

/// One profile write and one restart (S:§Journey 4). The plan is used once.
#[tauri::command]
pub async fn agent_ssh_apply(
    plans: State<'_, SshPlans>,
    jobs: State<'_, crate::override_restart::jobs::OverrideJobs>,
    plan_id: String,
    choices: Choices,
) -> Result<ApplyView, String> {
    let (approval, agent, hand_back) = plans
        .with(&plan_id, |h| {
            build_approval(&h.plan, &choices)
                .map(|a| (a, h.plan.agent.clone(), h.hand_back.clone()))
        })?
        .map_err(|e| format!("not ready: {e:?}"))?;
    jobs.refuse_if_busy(&agent)?;
    let exe = crate::fleet_pause::mur_binary()?;
    let run_agent = agent.clone();
    let applied = blocking(move || front::apply_for(&home(), &run_agent, &approval, exe)).await?;
    plans.remove(&plan_id);
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
/// this passes the CLI's `--yes`.
#[tauri::command]
pub async fn agent_ssh_revoke(
    name: String,
    key_fp: String,
    host_key_fp: Option<String>,
) -> Result<OverviewView, String> {
    let name = canonical(&name);
    blocking(move || {
        cmd_ssh_revoke(&SshRevokeArgs {
            agent: &name,
            key: Some(&key_fp),
            host_key: host_key_fp.as_deref(),
            yes: true,
        })
        .map_err(|e| format!("{e:#}"))?;
        let ov = front::overview(&System, &home(), &name).map_err(|e| format!("{e:#}"))?;
        Ok(view::overview_view(&name, &ov))
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
