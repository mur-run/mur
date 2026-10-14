//! `mur git-push` — the human's side of git push enrollment.
//!
//! `enroll` and `revoke` are the only writers of `enrollments.yaml`; both run read → check →
//! write under one exclusive lock so two shells cannot lose each other's record. `list` and
//! `doctor` only read. Every verb is `run_to` over an explicit writer so tests read what a
//! user reads.
//!
//! This CLI never opens the agent's repo: the repo lives in the agent's sandbox, so its
//! `.git/config` is agent-controlled, and this process runs with the human's authority.
//! `--object-format` is therefore stated by the human, and a wrong value fails closed in the
//! broker's oid validation and `index-pack`.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use fs2::FileExt;
use mur_common::agent::AgentProfile;
use mur_common::git_push::{broker_dir, enrollments_path, private_dir, registry_path};
use mur_git_broker::constants::ALLOWED_REF_PREFIX;
use mur_git_broker::doctor::{AgentFacts, EnrollmentProblem, check_enrollment};
use mur_git_broker::enrollment::{Endpoint, EnrollOutcome, EnrollRequest, Enrollments, NewRepo};
use mur_git_broker::oid::ObjectFormat;
use mur_git_broker::policy::RemotePolicy;
use serde::Serialize;

const LOCK_FILE: &str = "enrollments.lock";
const HEADS_PREFIX: &str = "refs/heads/";

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    Sha1,
    Sha256,
}
impl From<Format> for ObjectFormat {
    fn from(f: Format) -> Self {
        match f {
            Format::Sha1 => ObjectFormat::Sha1,
            Format::Sha256 => ObjectFormat::Sha256,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum GitPushAction {
    /// Allow an agent to ask for pushes to a remote. Refuses, and writes nothing, if the doctor objects.
    Enroll {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        repo: String,
        #[arg(long)]
        remote: String,
        /// `file:///absolute/path/to/remote.git`, or `https://<dns-name>[:<port>]/<path>`.
        #[arg(long)]
        endpoint: String,
        /// Full `refs/heads/<name>` an agent may create a branch from. Repeatable; default none.
        #[arg(long = "base-ref")]
        base_refs: Vec<String>,
        /// Branches the agent may push. Default: `refs/heads/agent/<agent>/`.
        #[arg(long)]
        ref_prefix: Option<String>,
        /// The agent's repo directory. Required the first time `--repo` names a new repo.
        #[arg(long)]
        path: Option<PathBuf>,
        /// Object format of that repo (only with `--path`). Default sha1.
        #[arg(long, value_enum)]
        object_format: Option<Format>,
    },
    /// Remove one enrollment. A repo with none left is removed too.
    Revoke {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        repo: String,
        #[arg(long)]
        remote: String,
    },
    /// Repoint a repo id. Voids every agent's pending pushes for it.
    #[command(name = "repo")]
    Repo {
        #[command(subcommand)]
        action: RepoAction,
    },
    /// Show active enrollments.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Re-run every enrollment check. Exit code 1 if any enrollment would be refused.
    Doctor {
        #[arg(long)]
        json: bool,
        /// Regenerate `registry.yaml` from `enrollments.yaml`.
        #[arg(long)]
        fix: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum RepoAction {
    /// Point an existing repo id at a new directory.
    Move {
        repo_id: String,
        #[arg(long)]
        path: PathBuf,
        #[arg(long, value_enum)]
        object_format: Option<Format>,
    },
}

pub fn run(mur_home: &Path, action: GitPushAction) -> Result<i32> {
    run_to(mur_home, action, &mut std::io::stdout())
}

pub fn run_to(mur_home: &Path, action: GitPushAction, out: &mut dyn Write) -> Result<i32> {
    match action {
        GitPushAction::Enroll {
            agent,
            repo,
            remote,
            endpoint,
            base_refs,
            ref_prefix,
            path,
            object_format,
        } => enroll(
            mur_home,
            out,
            EnrollArgs {
                agent,
                repo,
                remote,
                endpoint,
                base_refs,
                ref_prefix,
                path,
                object_format,
            },
        ),
        GitPushAction::Revoke {
            agent,
            repo,
            remote,
        } => revoke(mur_home, out, &agent, &repo, &remote),
        GitPushAction::Repo {
            action:
                RepoAction::Move {
                    repo_id,
                    path,
                    object_format,
                },
        } => move_repo(mur_home, out, &repo_id, path, object_format),
        GitPushAction::List { json } => list(mur_home, out, json),
        GitPushAction::Doctor { json, fix } => doctor(mur_home, out, json, fix),
    }
}

struct EnrollArgs {
    agent: String,
    repo: String,
    remote: String,
    endpoint: String,
    base_refs: Vec<String>,
    ref_prefix: Option<String>,
    path: Option<PathBuf>,
    object_format: Option<Format>,
}

/// Held for the whole read → check → write of a mutation. The file is never read: the
/// exclusive lock lives as long as the descriptor, and closing it releases the lock.
struct Locked(#[allow(dead_code)] std::fs::File);
impl Locked {
    fn acquire(mur_home: &Path) -> Result<Self> {
        let dir = private_dir(mur_home);
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(LOCK_FILE))
            .context("open enrollment lock")?;
        f.lock_exclusive().context("lock enrollments")?;
        Ok(Self(f))
    }
}

fn load(mur_home: &Path) -> Result<Option<Enrollments>> {
    Enrollments::load(&enrollments_path(mur_home)).map_err(|e| anyhow::anyhow!("{e}"))
}

/// A fresh file starts its epochs at the clock, so a deleted file can never reissue an epoch an
/// older pending row still holds.
fn fresh_enrollments() -> Enrollments {
    Enrollments::empty(chrono::Utc::now().timestamp().max(1) as u64)
}

/// Every agent's sandbox facts. An agent whose profile cannot be read counts as unsandboxed:
/// the doctor cannot vouch for it.
pub fn collect_agents(mur_home: &Path) -> Vec<AgentFacts> {
    let Ok(rd) = std::fs::read_dir(mur_home.join("agents")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter(|e| e.path().join("profile.yaml").is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names.into_iter().map(|n| facts_for(mur_home, &n)).collect()
}

fn facts_for(mur_home: &Path, name: &str) -> AgentFacts {
    let home = mur_home.join("agents").join(name);
    let enforcing = match mur_common::lock_file::read(&home.join("running.lock")) {
        Ok(Some(lock)) => lock.sandbox.map(|s| s.enforcing),
        _ => None,
    };
    match AgentProfile::load(mur_home, name) {
        Ok(p) => {
            let policy = mur_agent_runtime::sandbox::policy::SandboxPolicy::from_entitlements(
                &p.entitlements,
                &home,
            );
            let mut write_paths = policy.fs_write;
            write_paths.extend(mur_agent_runtime::supervisor::extra_write_paths(mur_home));
            AgentFacts {
                name: name.to_string(),
                home,
                write_paths,
                fail_closed: p.entitlements.fail_closed_on_sandbox_error,
                sandbox_enforcing: enforcing,
            }
        }
        Err(_) => AgentFacts {
            name: name.to_string(),
            home,
            write_paths: Vec::new(),
            fail_closed: false,
            sandbox_enforcing: enforcing,
        },
    }
}

fn describe(p: &EnrollmentProblem) -> String {
    use EnrollmentProblem::*;
    match p {
        BaseRefNotFullHead(r) => format!("--base-ref {r:?} is not a full refs/heads/<name>"),
        RefPrefixTooWide(r) => {
            format!("ref prefix {r:?} must be under {ALLOWED_REF_PREFIX} and end with '/'")
        }
        GrantCoversBrokerDir(g) => format!(
            "write grant {} overlaps the broker directory (the sandbox closes this; reported only)",
            g.display()
        ),
        EndpointNotCanonical(e) => format!(
            "endpoint {} is not a real path (a symlink or alias in it); give the resolved path",
            e.display()
        ),
        EndpointMissing(e) => format!("endpoint {} does not exist", e.display()),
        EndpointWritableByAgent { agent, path } => format!(
            "agent {agent} can write to {}, which contains or sits inside the remote; a hook \
             there would run with the broker's privileges",
            path.display()
        ),
        EndpointPathChecksNotApplicable(e) => format!(
            "endpoint {e} is a network remote: the path checks (canonical, exists, agent-writable) \
             do not apply to it, so they were not run"
        ),
        AgentMayRunUnsandboxed(a) => format!(
            "agent {a} may run without its sandbox (fail_closed_on_sandbox_error is false or its \
             sandbox is not enforcing) and could rewrite the enrollment"
        ),
    }
}

/// The agent's directory name exactly as it is on disk. The record's key must equal the name
/// intake reads from the directory, and on a case-insensitive filesystem
/// `canonicalize_agent_name` returns the typed name unchanged when it already resolves, so
/// match the listing instead.
fn on_disk_agent_name(mur_home: &Path, typed: &str) -> Option<String> {
    collect_agents(mur_home)
        .into_iter()
        .map(|a| a.name)
        .find(|n| n.eq_ignore_ascii_case(typed))
}

fn enroll(mur_home: &Path, out: &mut dyn Write, a: EnrollArgs) -> Result<i32> {
    let Some(agent) = on_disk_agent_name(mur_home, &a.agent) else {
        bail!("no agent named {:?}", a.agent);
    };
    for r in &a.base_refs {
        if !r.starts_with(HEADS_PREFIX) {
            bail!("--base-ref {r:?} must be a full ref; did you mean {HEADS_PREFIX}{r}?");
        }
    }
    if a.object_format.is_some() && a.path.is_none() {
        bail!("--object-format only applies with --path (a new repo, or `repo move`)");
    }
    let endpoint = Endpoint::parse(&a.endpoint).map_err(|_| {
        anyhow::anyhow!(
            "--endpoint must be file:///absolute/path or https://<dns-name>[:<port>]/<path> \
             (lowercase DNS name, no user@, no IP address, no explicit :443)"
        )
    })?;
    let ref_prefix = a
        .ref_prefix
        .unwrap_or_else(|| format!("{ALLOWED_REF_PREFIX}{agent}/"));
    let new_repo = a.path.map(|path| NewRepo {
        path,
        object_format: a.object_format.map_or(ObjectFormat::Sha1, Into::into),
    });

    let _lock = Locked::acquire(mur_home)?;
    let mut book = load(mur_home)?.unwrap_or_else(fresh_enrollments);
    let policy = RemotePolicy {
        remote_id: a.remote.clone(),
        canonical_remote_endpoint: endpoint.as_str().to_string(),
        creation_base_refs: a.base_refs.clone(),
        ref_prefix: ref_prefix.clone(),
        credential: None,
    };
    let problems = check_enrollment(
        &policy,
        &endpoint,
        &collect_agents(mur_home),
        &broker_dir(mur_home),
    );
    let (refusals, notes): (Vec<_>, Vec<_>) = problems.iter().partition(|p| p.is_refusal());
    for n in &notes {
        writeln!(out, "note: {}", describe(n))?;
    }
    if !refusals.is_empty() {
        for r in &refusals {
            writeln!(out, "refused: {}", describe(r))?;
        }
        writeln!(out, "nothing was written")?;
        return Ok(1);
    }
    let outcome = book
        .enroll(EnrollRequest {
            agent: agent.clone(),
            repo_id: a.repo.clone(),
            remote_id: a.remote.clone(),
            endpoint,
            ref_prefix,
            creation_base_refs: a.base_refs,
            credential: None,
            new_repo,
        })
        .map_err(|e| match e.to_string().as_str() {
            "invalid_request: repo_differs" => anyhow::anyhow!(
                "repo {:?} already points elsewhere; use `mur git-push repo move` (it voids every \
                 agent's pending pushes for that repo)",
                a.repo
            ),
            "invalid_request: repo_unknown" => anyhow::anyhow!(
                "repo {:?} is new: give --path <its directory> (and --object-format if not sha1)",
                a.repo
            ),
            other => anyhow::anyhow!("{other}"),
        })?;
    if outcome != EnrollOutcome::Unchanged {
        book.save(mur_home).map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    match outcome {
        EnrollOutcome::Created => writeln!(out, "enrolled {agent} for {}/{}", a.repo, a.remote)?,
        EnrollOutcome::Unchanged => writeln!(out, "already enrolled, nothing changed")?,
        EnrollOutcome::Updated => writeln!(
            out,
            "updated {agent} for {}/{}; pending pushes under the old policy become policy_changed",
            a.repo, a.remote
        )?,
    }
    Ok(0)
}

fn revoke(
    mur_home: &Path,
    out: &mut dyn Write,
    agent: &str,
    repo: &str,
    remote: &str,
) -> Result<i32> {
    let agent = on_disk_agent_name(mur_home, agent).unwrap_or_else(|| agent.to_string());
    let _lock = Locked::acquire(mur_home)?;
    let Some(mut book) = load(mur_home)? else {
        writeln!(out, "no enrollments")?;
        return Ok(1);
    };
    if !book.revoke(&agent, repo, remote) {
        writeln!(out, "{agent} is not enrolled for {repo}/{remote}")?;
        return Ok(1);
    }
    book.save(mur_home).map_err(|e| anyhow::anyhow!("{e}"))?;
    writeln!(out, "revoked {agent} for {repo}/{remote}")?;
    Ok(0)
}

fn move_repo(
    mur_home: &Path,
    out: &mut dyn Write,
    repo_id: &str,
    path: PathBuf,
    object_format: Option<Format>,
) -> Result<i32> {
    let _lock = Locked::acquire(mur_home)?;
    let Some(mut book) = load(mur_home)? else {
        writeln!(out, "no enrollments")?;
        return Ok(1);
    };
    let Some(current) = book.repos.get(repo_id) else {
        writeln!(out, "no repo {repo_id:?}")?;
        return Ok(1);
    };
    let fmt = object_format.map_or(current.object_format, Into::into);
    book.move_repo(
        repo_id,
        NewRepo {
            path,
            object_format: fmt,
        },
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    book.save(mur_home).map_err(|e| anyhow::anyhow!("{e}"))?;
    writeln!(
        out,
        "repo {repo_id} moved; pending pushes for it become policy_changed"
    )?;
    Ok(0)
}

#[derive(Serialize)]
struct Line<'a> {
    agent: &'a str,
    repo: &'a str,
    remote: &'a str,
    endpoint: &'a str,
    ref_prefix: &'a str,
    creation_base_refs: &'a [String],
    epoch: u64,
    repo_path: String,
}

fn list(mur_home: &Path, out: &mut dyn Write, json: bool) -> Result<i32> {
    let book = load(mur_home)?;
    let empty = fresh_enrollments();
    let book = book.as_ref().unwrap_or(&empty);
    let lines: Vec<Line> = book
        .enrollments
        .iter()
        .filter_map(|e| {
            let repo = book.repos.get(&e.repo_id)?;
            Some(Line {
                agent: &e.agent,
                repo: &e.repo_id,
                remote: &e.remote_id,
                endpoint: e.endpoint.as_str(),
                ref_prefix: &e.ref_prefix,
                creation_base_refs: &e.creation_base_refs,
                epoch: e.epoch,
                repo_path: repo.path.display().to_string(),
            })
        })
        .collect();
    if json {
        writeln!(out, "{}", serde_json::to_string_pretty(&lines)?)?;
    } else if lines.is_empty() {
        writeln!(out, "no enrollments")?;
    } else {
        for l in &lines {
            writeln!(
                out,
                "{} → {}/{}  {}  prefix {}  base {:?}",
                l.agent, l.repo, l.remote, l.endpoint, l.ref_prefix, l.creation_base_refs
            )?;
        }
    }
    Ok(0)
}

/// True if the router's signing key resolves: the same lookup the approval gate verifies with.
fn router_identity_present(mur_home: &Path) -> bool {
    crate::channel_verify::actor_pubkey(mur_home, &mur_common::channel::ChannelActor::System, None)
        .is_some()
}

#[derive(Serialize)]
struct Finding {
    agent: String,
    repo: String,
    remote: String,
    severity: &'static str,
    message: String,
}

fn doctor(mur_home: &Path, out: &mut dyn Write, json: bool, fix: bool) -> Result<i32> {
    let book = load(mur_home)?;
    let cfg = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml"));
    let agents = collect_agents(mur_home);
    let mut findings: Vec<Finding> = Vec::new();
    let mut refused = false;
    let empty = fresh_enrollments();
    let book_ref = book.as_ref().unwrap_or(&empty);
    let mut push = |agent: &str, repo: &str, remote: &str, severity, message: String| {
        findings.push(Finding {
            agent: agent.into(),
            repo: repo.into(),
            remote: remote.into(),
            severity,
            message,
        })
    };
    for e in &book_ref.enrollments {
        for p in check_enrollment(&e.policy(), &e.endpoint, &agents, &broker_dir(mur_home)) {
            if p.is_refusal() {
                refused = true;
            }
            let sev = if p.is_refusal() { "refuse" } else { "note" };
            push(&e.agent, &e.repo_id, &e.remote_id, sev, describe(&p));
        }
        if !agents.iter().any(|a| a.name == e.agent) {
            refused = true;
            push(
                &e.agent,
                &e.repo_id,
                &e.remote_id,
                "refuse",
                "agent does not exist".into(),
            );
        }
        if !cfg.git_push.enabled {
            push(
                &e.agent,
                &e.repo_id,
                &e.remote_id,
                "warn",
                "git_push.enabled is off in config.yaml; requests are ignored".into(),
            );
        } else if !cfg.git_push.agents.iter().any(|n| n == &e.agent) {
            push(
                &e.agent,
                &e.repo_id,
                &e.remote_id,
                "warn",
                "agent is not in git_push.agents; requests are refused".into(),
            );
        }
    }
    if !book_ref.enrollments.is_empty() && !router_identity_present(mur_home) {
        // Without it every approval request is written unsigned, and the gate counts only
        // router-signed ones: pushes would sit in `pending_approval` forever with no error.
        refused = true;
        push(
            "-",
            "-",
            "-",
            "refuse",
            format!(
                "no readable `{}` identity under agents/; approval requests cannot be signed, \
                 so no push can be approved",
                crate::channel_writer::ROUTER_AGENT
            ),
        );
    }
    let want = book_ref
        .registry_yaml()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let have = std::fs::read_to_string(registry_path(mur_home)).ok();
    let stale = have
        .as_deref()
        .map_or(!book_ref.enrollments.is_empty(), |h| h != want);
    if stale {
        if fix {
            book_ref
                .save(mur_home)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            push(
                "-",
                "-",
                "-",
                "fixed",
                "registry.yaml regenerated from enrollments.yaml".into(),
            );
        } else {
            push(
                "-",
                "-",
                "-",
                "warn",
                "registry.yaml differs from enrollments.yaml; run `mur git-push doctor --fix`"
                    .into(),
            );
        }
    }
    if json {
        writeln!(out, "{}", serde_json::to_string_pretty(&findings)?)?;
    } else if findings.is_empty() {
        writeln!(out, "ok")?;
    } else {
        for f in &findings {
            writeln!(
                out,
                "{:6} {}/{}/{}: {}",
                f.severity, f.agent, f.repo, f.remote, f.message
            )?;
        }
    }
    Ok(i32::from(refused))
}

#[cfg(test)]
#[path = "git_push_tests.rs"]
mod tests;
