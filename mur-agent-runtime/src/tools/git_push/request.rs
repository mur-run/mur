//! `git_push_request`: validate, build a pack in the agent's own repo, sign, drop.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use mur_common::identity::AgentIdentity;
use mur_git_broker::oid::{ObjectFormat, validate_oid, validate_ref, zero_oid};
use mur_git_broker::request_file::{
    RequestFile, RequestFileError, pack_path, read_request, request_path, sha256_file, validate_id,
    write_request,
};

use super::registry::GitPushRegistry;
use super::{GIT_PUSH_REQUEST, invalid};
use crate::llm::ToolDef;
use crate::tools::{ToolError, ToolExecutor, ToolOutput};

/// Everything the tool needs, supplied by the runtime. None of it is a tool argument.
pub struct GitPushCtx {
    pub agent: String,
    /// Fixed task id (tests). `None` in production: the id of the turn the call
    /// runs in, from the runtime's task scope; a call outside any turn is refused.
    pub task_id: Option<String>,
    /// `<agent_home>/inbox/git-push`.
    pub inbox: PathBuf,
    /// `<mur_home>/git-push/registry.yaml`, re-read on every call.
    pub registry_path: PathBuf,
    pub identity: Arc<AgentIdentity>,
    pub key_version: u32,
    pub now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
}

pub struct GitPushRequestTool {
    pub(super) ctx: GitPushCtx,
}

/// All `String`, none `Option`: a null or missing `old_sha` is a schema error.
/// `agent`, `task_id` and `enrollment_epoch` are absent, so supplying one is an
/// unknown field and is refused.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    request_id: String,
    repo_id: String,
    remote_id: String,
    r#ref: String,
    old_sha: String,
    new_sha: String,
}

impl GitPushRequestTool {
    pub fn new(ctx: GitPushCtx) -> Self {
        Self { ctx }
    }

    fn task_id(&self) -> Result<String, ToolError> {
        let id = self
            .ctx
            .task_id
            .clone()
            .or_else(crate::tools::bash_jobs::current_task_id)
            .ok_or_else(|| ToolError::Execution("git_push_request ran outside a task".into()))?;
        validate_id(&id, "task_id").map_err(|_| ToolError::Execution("task id unusable".into()))?;
        Ok(id)
    }
}

fn conflict() -> ToolError {
    ToolError::InvalidInput(
        "request_conflict: this request_id already names a different request; mint a new id".into(),
    )
}

fn pending(id: &str) -> ToolOutput {
    format!(
        "request {id}: pending — submitted to the git push broker. A human must approve it before \
anything is pushed. Check with git_push_status {{\"request_id\":\"{id}\"}}; reuse this request_id \
for every status query, retry and cancel."
    )
    .into()
}

/// Git in the agent's own repo, inside the agent's sandbox. The broker never runs here.
async fn git(
    repo: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
) -> Result<std::process::Output, ToolError> {
    let mut c = Command::new("git");
    c.arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .kill_on_drop(true);
    c.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = c
        .spawn()
        .map_err(|e| ToolError::Execution(format!("git: {}", e.kind())))?;
    if let (Some(bytes), Some(mut w)) = (stdin, child.stdin.take()) {
        w.write_all(bytes)
            .await
            .map_err(|e| ToolError::Execution(format!("git stdin: {}", e.kind())))?;
    }
    child
        .wait_with_output()
        .await
        .map_err(|e| ToolError::Execution(format!("git: {}", e.kind())))
}

async fn object_format(repo: &Path) -> Result<ObjectFormat, ToolError> {
    let o = git(repo, &["rev-parse", "--show-object-format"], None).await?;
    match (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).trim(),
    ) {
        (true, "sha1") => Ok(ObjectFormat::Sha1),
        (true, "sha256") => Ok(ObjectFormat::Sha256),
        // No path in the message: premise 2, the mapping is never echoed.
        _ => Err(invalid("repo unavailable")),
    }
}

async fn has_commit(repo: &Path, oid: &str) -> Result<bool, ToolError> {
    Ok(git(
        repo,
        &["cat-file", "-e", &format!("{oid}^{{commit}}")],
        None,
    )
    .await?
    .status
    .success())
}

impl GitPushRequestTool {
    async fn run(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let a: Args =
            serde_json::from_value(input).map_err(|e| invalid(&format!("schema: {e}")))?;
        for (v, what) in [
            (&a.request_id, "request_id"),
            (&a.repo_id, "repo_id"),
            (&a.remote_id, "remote_id"),
        ] {
            validate_id(v, what).map_err(|_| invalid(what))?;
        }
        validate_ref(&a.r#ref)
            .map_err(|_| invalid("ref must be a full refs/heads/agent/* name"))?;

        let registry =
            GitPushRegistry::load(&self.ctx.registry_path).map_err(ToolError::Execution)?;
        let entry = registry
            .get(&a.repo_id)
            .ok_or_else(|| invalid("unknown repo_id"))?;
        if !entry.remotes.iter().any(|r| r == &a.remote_id) {
            return Err(invalid("remote_id is not enrolled for this repo"));
        }
        let repo = entry.path.clone();

        let fmt = object_format(&repo).await?;
        validate_oid(&a.old_sha, fmt)
            .map_err(|_| invalid("old_sha must be a full lowercase oid"))?;
        validate_oid(&a.new_sha, fmt)
            .map_err(|_| invalid("new_sha must be a full lowercase oid"))?;
        let zero = zero_oid(fmt);
        if a.new_sha == zero {
            return Err(invalid("deleting a ref is not supported"));
        }
        let creation = a.old_sha == zero;

        let task_id = self.task_id()?;
        let mut req = RequestFile {
            agent: self.ctx.agent.clone(),
            task_id,
            request_id: a.request_id,
            repo_id: a.repo_id,
            remote_id: a.remote_id,
            r#ref: a.r#ref,
            old_sha: a.old_sha,
            new_sha: a.new_sha,
            pack_sha256: String::new(),
            requested_at: (self.ctx.now)(),
            key_version: self.ctx.key_version,
            sig: String::new(),
        };
        // Idempotency before any work: same id + same content ⇒ already submitted.
        let existing = request_path(&self.ctx.inbox, &req.request_id);
        if existing.exists() {
            let prior = read_request(&existing).map_err(|e| ToolError::Execution(e.to_string()))?;
            return if prior.same_request_as(&req) {
                Ok(pending(&req.request_id))
            } else {
                Err(conflict())
            };
        }

        if !has_commit(&repo, &req.new_sha).await?
            || (!creation && !has_commit(&repo, &req.old_sha).await?)
        {
            return Err(invalid("an oid is not a commit in this repo"));
        }

        // `new ^old`: the broker prefetches old from the remote itself (F33).
        let revs = if creation {
            format!("{}\n", req.new_sha)
        } else {
            format!("{}\n^{}\n", req.new_sha, req.old_sha)
        };
        let o = git(
            &repo,
            &["pack-objects", "--revs", "--stdout"],
            Some(revs.as_bytes()),
        )
        .await?;
        if !o.status.success() {
            return Err(ToolError::Execution("git pack-objects failed".into()));
        }
        std::fs::create_dir_all(&self.ctx.inbox)
            .map_err(|e| ToolError::Execution(format!("inbox: {}", e.kind())))?;
        let pack = pack_path(&self.ctx.inbox, &req.request_id);
        let tmp = pack.with_extension("pack.tmp");
        std::fs::write(&tmp, &o.stdout)
            .and_then(|_| std::fs::rename(&tmp, &pack))
            .map_err(|e| ToolError::Execution(format!("pack: {}", e.kind())))?;
        req.pack_sha256 = sha256_file(&pack).map_err(|e| ToolError::Execution(e.to_string()))?;
        req.sig = self.ctx.identity.sign_multibase(&req.sign_input());

        match write_request(&self.ctx.inbox, &req) {
            Ok(_) => Ok(pending(&req.request_id)),
            Err(RequestFileError::Conflict) => Err(conflict()),
            Err(e) => Err(ToolError::Execution(e.to_string())),
        }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for GitPushRequestTool {
    fn name(&self) -> &str {
        GIT_PUSH_REQUEST
    }

    fn def(&self) -> ToolDef {
        let id = |d: &str| serde_json::json!({"type": "string", "description": d});
        ToolDef {
            name: GIT_PUSH_REQUEST.into(),
            description: "Ask the git push broker to push one commit to an agent branch. You \
never push yourself: this packs the commits from your repo, signs the request and queues it; a \
human approves the exact (ref, old, new) before the broker pushes. Returns `pending` at once. \
Mint `request_id` yourself before the first call and reuse it for every retry, status query and \
cancel — same id with different content is refused as request_conflict."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "request_id": id("Your idempotency key: letters, digits, '.', '-', '_'; max 128."),
                    "repo_id": id("Name of a repo a human enrolled for pushing. A name, never a path."),
                    "remote_id": id("Name of a remote enrolled for that repo. A name, never a URL."),
                    "ref": id("Full ref under refs/heads/agent/, e.g. refs/heads/agent/task-123."),
                    "old_sha": id("Full oid the remote ref points at now. For a new branch, the all-zero oid (40 zeros for SHA-1, 64 for SHA-256)."),
                    "new_sha": id("Full oid of the commit to push.")
                },
                "required": ["request_id", "repo_id", "remote_id", "ref", "old_sha", "new_sha"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        self.run(input).await
    }
}
