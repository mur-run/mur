//! Pre-dispatch write-grant gate: may each member write where it is being
//! sent? Shared by `mur fleet run` and `parallel_jobs` so N jobs to one member
//! raise ONE approval, and so a member never learns it lacks the grant by
//! building in the wrong tree (#1607). Design:
//! `docs/superpowers/specs/2026-10-01-delegation-write-grant-design.md` §4.2.
//!
//! The rules, in the order they apply per unique `(member, dir)`:
//!
//! 1. `dir` must exist — a grant for a missing directory is accepted by the
//!    profile and dropped by the sandbox at start, so it is refused here.
//! 2. `deny` is literal and final: a denied target is blocked with no offer.
//! 3. `allowed && !inferred` is the silent fast path.
//! 4. Everything else goes through the channel HITL gate as a `write`-tier
//!    action. Unattended it DEFERS (parked request, dispatch blocked); it never
//!    times out into a grant. `--yes` and a `write` tier pre-approval satisfy
//!    it, capped by `tier_may_be_granted` like every other gate.
//! 5. Approved and not yet allowed: append to `filesystem.write`, save (which
//!    advances the entitlements pin), then restart — but only a member that
//!    runs as a managed service. One that was started by hand is never
//!    respawned by MUR: the dispatch is blocked and the user is told to run
//!    `mur agent restart <member>`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use mur_agent_runtime::entitlements::{WriteVerdict, write_verdict};
use mur_agent_runtime::sandbox::launch_chain::{LaunchChain, is_overbroad_grant_root};
use mur_common::channel::{ChannelActor, EventKind};
use mur_common::hitl::RiskTier;

use crate::channel_writer::ROUTER_AGENT;
use crate::hitl::gate::{ActionRequest, GatePolicy, gate};

/// One member and the directory it is about to be sent to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DelegationTarget {
    pub member: String,
    /// Canonical.
    pub dir: PathBuf,
    /// The directory was guessed from the caller's session cwd, not stated.
    /// Always asks, even when the member may already write there (D2).
    pub inferred: bool,
}

/// Why a dispatch must not proceed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockReason {
    /// The directory does not exist.
    TargetMissing,
    /// Under the member's `filesystem.deny`. Never offered for grant.
    Denied,
    /// Under MUR's launch chain or too broad to ever grant.
    Ungrantable(String),
    /// The member's profile could not be read.
    Profile(String),
    /// Nobody answered and nobody was made to wait: the request is parked on
    /// the channel; `mur channel approve <channel> <hitl_id>` releases it.
    Deferred { hitl_id: String },
    /// A human (or policy) said no.
    Refused(String),
    /// The grant is written and sealed, but the member is running outside a
    /// service, so MUR will not restart it (D3b).
    RestartRequired,
    /// The member runs as a service; the restart did not confirm a new pid.
    RestartFailed(String),
}

/// The gate's verdict for one target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantOutcome {
    /// Nothing written; dispatch may proceed.
    AlreadyAllowed,
    /// Profile edited + resealed. `restarted` is false only when the member
    /// was not running at all, so its next start applies the grant.
    Granted { restarted: bool },
    /// Dispatch must not proceed.
    Blocked(BlockReason),
}

impl GrantOutcome {
    pub fn blocks(&self) -> bool {
        matches!(self, GrantOutcome::Blocked(_))
    }
}

/// Where the gate parks and answers its approvals.
pub struct GrantContext<'a> {
    pub mur_home: &'a Path,
    pub channel_id: &'a str,
    pub run_id: &'a str,
    pub policy: GatePolicy,
    /// How many jobs this run will dispatch — the prompt says so, because the
    /// user is signing a dispatch, not just a profile edit.
    pub job_count: usize,
}

/// Build the unique target list for a fan-out: one entry per member, all
/// routed to `dir`. `members` may repeat (N jobs to one member).
pub fn targets_for<'m>(
    members: impl IntoIterator<Item = &'m str>,
    dir: &Path,
    inferred: bool,
) -> Vec<DelegationTarget> {
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut seen = HashSet::new();
    members
        .into_iter()
        .filter(|m| seen.insert(m.to_string()))
        .map(|m| DelegationTarget {
            member: m.to_string(),
            dir: dir.clone(),
            inferred,
        })
        .collect()
}

/// Run the gate for every target. Returns one outcome per target, in order;
/// the caller must not dispatch if any `blocks()`. Each blocked outcome is
/// also recorded on the channel as a `delegation.write_grant` note.
pub async fn ensure_write_grants(
    ctx: &GrantContext<'_>,
    targets: &[DelegationTarget],
) -> Result<Vec<(DelegationTarget, GrantOutcome)>> {
    let mut out = Vec::with_capacity(targets.len());
    for t in targets {
        let outcome = ensure_one(ctx, t).await?;
        tracing::info!(
            member = %t.member,
            dir = %t.dir.display(),
            inferred = t.inferred,
            outcome = ?outcome,
            "delegation.write_grant"
        );
        record(ctx, t, &outcome);
        out.push((t.clone(), outcome));
    }
    Ok(out)
}

/// The text a caller bails with when something blocked: every blocked target
/// with its fix line. `None` when nothing blocked.
pub fn block_message(
    results: &[(DelegationTarget, GrantOutcome)],
    channel_id: &str,
) -> Option<String> {
    let mut lines = Vec::new();
    for (t, o) in results {
        let GrantOutcome::Blocked(reason) = o else {
            continue;
        };
        let dir = t.dir.display();
        let member = &t.member;
        let line = match reason {
            BlockReason::TargetMissing => {
                format!("{member}: target `{dir}` does not exist; pass cwd=<an existing directory>")
            }
            BlockReason::Denied => format!(
                "{member}: `{dir}` is under its filesystem.deny list; MUR will not grant past a deny. \
                 Pass a different cwd or edit the profile by hand."
            ),
            BlockReason::Ungrantable(why) => format!("{member}: `{dir}` cannot be granted: {why}"),
            BlockReason::Profile(why) => format!("{member}: cannot read profile: {why}"),
            BlockReason::Deferred { hitl_id } => format!(
                "{member}: needs approval to write `{dir}` — parked. \
                 Approve with `mur channel approve {channel_id} {hitl_id}`, then re-run."
            ),
            BlockReason::Refused(why) => {
                let hint = if t.inferred {
                    "; pass cwd=<the target project> to state the directory explicitly"
                } else {
                    ""
                };
                format!("{member}: write to `{dir}` refused ({why}){hint}")
            }
            BlockReason::RestartRequired => format!(
                "{member}: `{dir}` granted and resealed, but {member} is not running as a service, \
                 so MUR did not restart it. Run `mur agent restart {member}`, then re-run."
            ),
            BlockReason::RestartFailed(why) => format!(
                "{member}: `{dir}` granted and resealed; restart did not confirm a new pid ({why}). \
                 Check `mur agent status {member}`, then re-run."
            ),
        };
        lines.push(line);
    }
    if lines.is_empty() {
        None
    } else {
        Some(format!(
            "delegation blocked before dispatch:\n  {}",
            lines.join("\n  ")
        ))
    }
}

async fn ensure_one(ctx: &GrantContext<'_>, t: &DelegationTarget) -> Result<GrantOutcome> {
    use GrantOutcome::Blocked;
    let profile_path = profile_path(ctx.mur_home, &t.member);
    let profile = match load_profile(&profile_path) {
        Ok(p) => p,
        Err(e) => return Ok(Blocked(BlockReason::Profile(format!("{e:#}")))),
    };
    let verdict = write_verdict(&profile.entitlements.filesystem, &t.dir);
    let allowed = match verdict {
        WriteVerdict::Missing => return Ok(Blocked(BlockReason::TargetMissing)),
        WriteVerdict::Denied => return Ok(Blocked(BlockReason::Denied)),
        WriteVerdict::Allowed => true,
        WriteVerdict::NotGranted => false,
    };
    if allowed && !t.inferred {
        return Ok(GrantOutcome::AlreadyAllowed);
    }
    // Refuse what could never be granted BEFORE asking, so the user is not
    // asked to approve a grant that the profile writer would then reject.
    if !allowed && let Some(why) = ungrantable(ctx.mur_home, &t.member, &t.dir) {
        return Ok(Blocked(BlockReason::Ungrantable(why)));
    }
    let service = crate::cmd::agent::installed_service(&t.member).is_some();
    let running = member_running(ctx.mur_home, &t.member);

    let req = ActionRequest {
        tier: RiskTier::Write,
        tool_name: "delegation.write_grant".into(),
        tool_input: serde_json::json!({
            "member": t.member,
            "dir": t.dir,
            "inferred": t.inferred,
            "service": service,
            "grant": !allowed,
        }),
        step_or_call_id: format!("grant:{}", t.member),
        agent_id: ROUTER_AGENT.into(),
        summary: prompt(t, allowed, service, ctx.job_count),
    };
    let decision = gate(
        ctx.mur_home,
        ctx.channel_id,
        &req,
        &ctx.policy,
        None,
        Some(ctx.run_id),
    )
    .await
    .context("write-grant gate")?;
    if decision.deferred {
        return Ok(Blocked(BlockReason::Deferred {
            hitl_id: decision.hitl_id.unwrap_or_default(),
        }));
    }
    if !decision.allow {
        return Ok(Blocked(BlockReason::Refused(decision.reason)));
    }
    if allowed {
        // Inferred-but-allowed: the human confirmed the directory; nothing to
        // write.
        return Ok(GrantOutcome::AlreadyAllowed);
    }

    write_grant(&profile_path, profile, &t.dir)?;
    if !running {
        return Ok(GrantOutcome::Granted { restarted: false });
    }
    if !service {
        return Ok(Blocked(BlockReason::RestartRequired));
    }
    match crate::cmd::agent::restart_quiet(&t.member) {
        Ok(r) if r.ok => Ok(GrantOutcome::Granted { restarted: true }),
        Ok(r) => Ok(Blocked(BlockReason::RestartFailed(r.detail))),
        Err(e) => Ok(Blocked(BlockReason::RestartFailed(e.to_string()))),
    }
}

/// §4.3: the user signs a COMPLETE action — grant, restart (or not), and the
/// dispatch it unlocks — in one gate, never two.
fn prompt(t: &DelegationTarget, allowed: bool, service: bool, jobs: usize) -> String {
    let (member, dir) = (&t.member, t.dir.display());
    let inferred = if t.inferred {
        format!(
            "No target directory was given for this delegation. Inferred `{dir}` from the \
             session working directory — dispatch {jobs} job(s) against it?"
        )
    } else {
        String::new()
    };
    if allowed {
        return inferred;
    }
    let grant = if service {
        format!(
            "Add `{dir}` to {member}'s filesystem write list and restart {member} (waits for \
             its current turn to finish)? Required to dispatch {jobs} job(s) for this run."
        )
    } else {
        format!(
            "Add `{dir}` to {member}'s filesystem write list. {member} is not running as a \
             service, so MUR will not restart it: after resealing you must run \
             `mur agent restart {member}` yourself, and this dispatch will fail. Continue?"
        )
    };
    if inferred.is_empty() {
        grant
    } else {
        format!("{inferred} {grant}")
    }
}

fn profile_path(mur_home: &Path, member: &str) -> PathBuf {
    mur_home.join("agents").join(member).join("profile.yaml")
}

fn load_profile(path: &Path) -> Result<mur_common::AgentProfile> {
    let yaml = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_yaml_ng::from_str(&yaml).with_context(|| format!("parse {}", path.display()))
}

/// The same two refusals `mur agent perm allow-write` applies, with an
/// explicit `mur_home` so the check agrees with the profile being edited.
fn ungrantable(mur_home: &Path, member: &str, dir: &Path) -> Option<String> {
    let chain = LaunchChain::new(&mur_home.join("agents").join(member));
    if let Some(reason) = chain.protects_write(dir) {
        return Some(format!("part of MUR's launch chain: {reason}"));
    }
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    if is_overbroad_grant_root(dir, &home) {
        return Some(
            "too broad — it covers the whole machine, the whole home directory, or a volume root"
                .into(),
        );
    }
    None
}

fn member_running(mur_home: &Path, member: &str) -> bool {
    let lock = mur_home.join("agents").join(member).join("running.lock");
    std::fs::read(&lock)
        .ok()
        .and_then(|b| serde_json::from_slice::<mur_common::LockFile>(&b).ok())
        .is_some_and(|l| mur_common::lock_file::pid_alive(l.pid))
}

/// Append `dir` to `filesystem.write` and save through the one profile
/// writer, which advances the entitlements pin (#712) and versions the change.
fn write_grant(path: &Path, mut profile: mur_common::AgentProfile, dir: &Path) -> Result<()> {
    let entry = dir.to_string_lossy().to_string();
    let write = &mut profile.entitlements.filesystem.write;
    if !write.iter().any(|p| p == &entry) {
        write.push(entry);
    }
    crate::cmd::agent::save_profile(path, &mut profile)
}

fn record(ctx: &GrantContext<'_>, t: &DelegationTarget, outcome: &GrantOutcome) {
    let outcome_label = match outcome {
        GrantOutcome::AlreadyAllowed => "already_allowed".to_string(),
        GrantOutcome::Granted { restarted } => format!("granted(restarted={restarted})"),
        GrantOutcome::Blocked(r) => format!("blocked({r:?})"),
    };
    let payload = serde_json::json!({
        "kind": "delegation.write_grant",
        "member": t.member,
        "dir": t.dir,
        "inferred": t.inferred,
        "outcome": outcome_label,
    });
    let _ = mur_channel::ChannelService::open(ctx.mur_home).and_then(|svc| {
        crate::channel_writer::append_as_writer(
            &svc,
            ctx.mur_home,
            ctx.channel_id,
            ROUTER_AGENT,
            ChannelActor::System,
            EventKind::Note,
            payload,
            None,
        )
    });
}

#[cfg(test)]
#[path = "grant_tests.rs"]
mod tests;
