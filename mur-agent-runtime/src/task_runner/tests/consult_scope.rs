//! Channel consult C4 (§D4, R4, R10): a consult scope reaches both read entry
//! points of a real turn — `read_file` through the loop, and the pinned
//! project instructions — and lives only as long as that one turn.

use super::*;
use crate::task_runner::consult_scope::ConsultScope;
use crate::tools::fs_policy::{SessionCwd, file_tool_policy};
use mur_common::agent::FilesystemEntitlement;
use std::path::{Path, PathBuf};

/// Stateless, so overlapping turns on one runner cannot cross-feed: a user
/// text `read:<path>` gets one `read_file` call on `<path>`, and the tool's
/// result comes back as the reply. `slow:` never answers. When `gate` is set,
/// every first call waits there, so two turns are provably in flight at once.
struct Reader {
    gate: Option<Arc<tokio::sync::Barrier>>,
    /// Every user text the model was shown, pinned instructions included.
    shown: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl crate::llm::LlmClient for Reader {
    async fn generate(
        &self,
        req: crate::llm::LlmRequest,
    ) -> Result<crate::llm::LlmResponse, crate::llm::LlmError> {
        if let Some(crate::llm::RichMessage::ToolResults { results }) = req.messages.last() {
            return Ok(end_turn_response(&results[0].content));
        }
        let texts: Vec<String> = req
            .messages
            .iter()
            .filter_map(|m| match m {
                crate::llm::RichMessage::Text { role, content } if role == "user" => {
                    Some(content.clone())
                }
                _ => None,
            })
            .collect();
        self.shown.lock().unwrap().extend(texts.iter().cloned());
        let input = texts.last().cloned().unwrap_or_default();
        if input.starts_with("slow:") {
            std::future::pending::<()>().await;
        }
        if let Some(g) = &self.gate {
            g.wait().await;
        }
        let Some(path) = input.strip_prefix("read:") else {
            return Ok(end_turn_response("no read"));
        };
        let mut call = tool_call_response("r-0", "");
        call.tool_calls[0].tool_name = crate::tools::read_file::READ_FILE.into();
        call.tool_calls[0].input = serde_json::json!({ "path": path });
        Ok(call)
    }
    fn model_name(&self) -> &str {
        "reader"
    }
}

/// `tw` answers; `pm` and `rv` ask. Every file's content is its own name.
struct World {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    tw: PathBuf,
}

impl World {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let tw = root.join("mur/agents/tw");
        std::fs::create_dir_all(&tw).unwrap();
        Self {
            _tmp: tmp,
            root,
            tw,
        }
    }

    fn file(&self, rel: &str) -> PathBuf {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, rel).unwrap();
        p
    }

    fn scope(&self, asker: &str, read: &[&str]) -> Arc<ConsultScope> {
        let home = self.root.join("mur/agents").join(asker);
        let (fs, chain) = file_tool_policy(self.ent(read), &home);
        Arc::new(ConsultScope::new(asker, fs, chain))
    }

    fn ent(&self, read: &[&str]) -> FilesystemEntitlement {
        FilesystemEntitlement {
            read: read
                .iter()
                .map(|r| self.root.join(r).to_string_lossy().into_owned())
                .collect(),
            write: vec![],
            deny: vec![],
        }
    }

    /// `tw`'s runner: it can read everything under `docs/` and `proj/`.
    fn runner(
        &self,
        gate: Option<Arc<tokio::sync::Barrier>>,
    ) -> (Arc<TaskRunner>, Arc<Mutex<Vec<String>>>) {
        let shown = Arc::new(Mutex::new(Vec::new()));
        let (fs, chain) = file_tool_policy(self.ent(&["docs", "proj"]), &self.tw);
        let cwd = SessionCwd::new(self.tw.clone());
        let pi = crate::project_instructions::ProjectInstructions::new(fs.clone(), chain.clone());
        let read = crate::tools::read_file::ReadFileTool::new(cwd.clone(), fs, chain);
        let runner = TaskRunner::with_llm(Arc::new(Reader {
            gate,
            shown: shown.clone(),
        }))
        .with_tools(vec![Arc::new(read)])
        .with_tools_policy(vec![mur_common::agent::ToolRule {
            pattern: "*".into(),
            policy: mur_common::agent::ToolPolicy::Allow,
            risk: None,
            bypass_builtin_tier_floor: Default::default(),
        }])
        .with_pending_approvals(empty_pending_approvals())
        .with_notifier(tokio::sync::mpsc::channel(64).0)
        .with_session_cwd(cwd, vec![self.root.to_string_lossy().into_owned()])
        .with_project_instructions(pi);
        (Arc::new(runner), shown)
    }
}

fn read_spec(path: &Path, scope: Option<Arc<ConsultScope>>) -> TaskSpec {
    TaskSpec {
        consult_scope: scope,
        ..loop_spec(&format!("read:{}", path.display()))
    }
}

fn reply(outcome: &TaskOutcome) -> String {
    let TaskOutcome::Completed(task) = outcome else {
        panic!("expected Completed, got {outcome:?}");
    };
    match task.messages.last().and_then(|m| m.parts.first()) {
        Some(MessagePart::Text { text }) => text.clone(),
        other => panic!("no text reply: {other:?}"),
    }
}

fn refused(r: &str) -> bool {
    r.contains("outside this consult's read scope")
}

/// R4 "Asker lacks a path the answerer has", through the real loop.
#[tokio::test]
async fn read_file_in_a_consult_turn_is_the_intersection() {
    let w = World::new();
    let internal = w.file("docs/internal/x.md");
    let shared = w.file("proj/a.md");
    let (runner, _) = w.runner(None);
    let pm = w.scope("pm", &["proj"]);
    let r = reply(
        &runner
            .run_sync(read_spec(&internal, Some(pm.clone())))
            .await,
    );
    assert!(refused(&r) && r.contains("`pm`"), "{r}");
    let r = reply(&runner.run_sync(read_spec(&shared, Some(pm))).await);
    assert_eq!(r, "proj/a.md");
}

/// R4 "Project instructions": a file the answerer can read and the asker
/// cannot is not rendered into the consult turn's pinned block, nor named.
#[tokio::test]
async fn project_instructions_follow_the_consult_scope() {
    let w = World::new();
    std::fs::create_dir_all(w.root.join("docs/repo/.git")).unwrap();
    std::fs::write(w.root.join("docs/repo/AGENTS.md"), "SECRET-RULES").unwrap();
    let repo = w.root.join("docs/repo");
    let (runner, shown) = w.runner(None);
    let spec = |scope| TaskSpec {
        cwd: Some(repo.clone()),
        consult_scope: scope,
        ..loop_spec("hello")
    };

    let _ = runner.run_sync(spec(Some(w.scope("pm", &["proj"])))).await;
    let consult = shown.lock().unwrap().join("\n");
    assert!(!consult.contains("SECRET-RULES"), "{consult}");
    assert!(!consult.contains("AGENTS.md"), "not even named: {consult}");

    shown.lock().unwrap().clear();
    let _ = runner.run_sync(spec(None)).await;
    let ordinary = shown.lock().unwrap().join("\n");
    assert!(
        ordinary.contains("SECRET-RULES"),
        "the answerer's own turn loads it"
    );
}

/// R10 "Consult beside an ordinary turn": both in flight on one runner.
#[tokio::test]
async fn consult_beside_an_ordinary_turn() {
    let w = World::new();
    let internal = w.file("docs/internal/x.md");
    let (runner, _) = w.runner(Some(Arc::new(tokio::sync::Barrier::new(2))));
    let (ordinary, consult) = tokio::join!(
        runner.run_sync(read_spec(&internal, None)),
        runner.run_sync(read_spec(&internal, Some(w.scope("pm", &["proj"])))),
    );
    assert_eq!(reply(&ordinary), "docs/internal/x.md");
    assert!(refused(&reply(&consult)));
}

/// R10 "Two consults with different askers": each gets only its own scope.
#[tokio::test]
async fn two_consults_with_different_askers() {
    let w = World::new();
    let internal = w.file("docs/internal/x.md");
    let (runner, _) = w.runner(Some(Arc::new(tokio::sync::Barrier::new(2))));
    let (pm, rv) = tokio::join!(
        runner.run_sync(read_spec(&internal, Some(w.scope("pm", &["proj"])))),
        runner.run_sync(read_spec(&internal, Some(w.scope("rv", &["docs"])))),
    );
    let pm = reply(&pm);
    assert!(refused(&pm) && pm.contains("`pm`"), "{pm}");
    assert_eq!(reply(&rv), "docs/internal/x.md");
}

/// R10 "Cancelled consult": the next ordinary turn reads with the full policy.
#[tokio::test]
async fn cancelled_consult_leaves_nothing_behind() {
    let w = World::new();
    let internal = w.file("docs/internal/x.md");
    let (runner, _) = w.runner(None);
    let spec = TaskSpec {
        task_id: Some("t-consult".into()),
        consult_scope: Some(w.scope("pm", &["proj"])),
        ..loop_spec("slow:")
    };
    let r2 = runner.clone();
    let handle = tokio::spawn(async move { r2.run_sync(spec).await });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    runner.cancel("t-consult").await.expect("cancel");
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("cancel ends the turn")
        .expect("join");
    assert!(matches!(outcome, TaskOutcome::Cancelled(_)), "{outcome:?}");
    let r = reply(&runner.run_sync(read_spec(&internal, None)).await);
    assert_eq!(r, "docs/internal/x.md");
}

/// R9: a scope alone, without an allowlist, is still refused off the loop.
#[tokio::test]
async fn cli_backend_refuses_a_scoped_turn() {
    let w = World::new();
    let runner = TaskRunner::with_cli_spawn(&mur_common::cli_backend::CLAUDE);
    let spec = TaskSpec {
        consult_scope: Some(w.scope("pm", &[])),
        ..loop_spec("consult")
    };
    let TaskOutcome::Failed(task) = runner.run_sync(spec).await else {
        panic!("a scoped turn must not run on a CLI backend");
    };
    assert_eq!(task.error.expect("error").code, "backend_cannot_confine");
}
