use super::*;
use crate::llm::stub::SequenceLlm;

fn git_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let td = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(td.path()).unwrap();
    let git = |args: &[&str]| {
        let st = std::process::Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .current_dir(&root)
            .status()
            .unwrap();
        assert!(st.success());
    };
    git(&["init", "-q"]);
    std::fs::write(root.join("a.txt"), "a\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "init"]);
    (td, root)
}

/// The whole reason tracks are eager (spec §4.1): the model is handed the
/// track as its working directory, so a `bash` redirect — which no file
/// tool sees — goes to the track, reaches the project only through
/// `promote`, and is counted in `~ changed` from the track's diff.
#[tokio::test]
async fn bash_write_in_a_turn_lands_via_promote_and_is_counted() {
    let (_td, project) = git_repo();
    let cwd = crate::tools::fs_policy::SessionCwd::new(project.clone());
    let bash: Arc<dyn crate::tools::ToolExecutor> = Arc::new(crate::tools::bash::BashTool::new(
        project.clone(),
        cwd.clone(),
    ));
    // `pwd` is what the model would see; the redirect is the write no file
    // tool records. Both relative: the prompt's working directory is the
    // track, and this is the model following it.
    let responses = vec![
        tool_call_response("c1", "pwd > where.txt; printf b >> a.txt"),
        end_turn_response("done"),
    ];
    let runner = TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
        .with_tools(vec![bash])
        .with_tools_policy(vec![mur_common::agent::ToolRule {
            pattern: "bash".into(),
            policy: mur_common::agent::ToolPolicy::Allow,
            risk: None,
        }])
        .with_sandbox_enforcing(true)
        .with_pending_approvals(empty_pending_approvals())
        .with_notifier(tokio::sync::mpsc::channel(16).0)
        .with_session_cwd(cwd.clone(), vec![project.to_string_lossy().into_owned()]);
    let mut spec = loop_spec("edit");
    spec.task_id = Some("turn-e2e".into());
    spec.cwd = Some(project.clone());

    let out = runner.run_sync(spec).await;
    let TaskOutcome::Completed(task) = out else {
        panic!("{out:?}");
    };

    // Edits reached the project — through promote, since the shell ran in
    // the track (its recorded pwd is under .worktrees/).
    assert_eq!(
        std::fs::read_to_string(project.join("a.txt")).unwrap(),
        "a\nb"
    );
    let where_ = std::fs::read_to_string(project.join("where.txt")).unwrap();
    assert!(
        where_.contains("/.worktrees/turn-"),
        "shell ran in the track, not the project: {where_}"
    );
    assert!(
        !project.join(".worktrees").join("turn-turn-e2e").exists(),
        "track destroyed after promote"
    );
    assert_eq!(
        cwd.for_turn("turn-e2e"),
        project,
        "cwd restored for the next turn"
    );

    // The card counts BOTH files from the track diff, though no file tool
    // ran — the per-action ledger alone would have said nothing changed.
    let text = task
        .messages
        .last()
        .and_then(|m| {
            m.parts.iter().find_map(|p| match p {
                MessagePart::Text { text } => Some(text.clone()),
                _ => None,
            })
        })
        .unwrap();
    assert!(text.contains("~ changed    2 file(s)"), "{text}");
    assert!(text.contains("      a.txt\n"), "{text}");
    assert!(text.contains("      where.txt\n"), "{text}");
    let ledger: crate::turn_ledger::TurnLedger = task
        .messages
        .last()
        .and_then(|m| {
            m.parts.iter().find_map(|p| match p {
                MessagePart::Data { data, .. } => serde_json::from_value(data.clone()).ok(),
                _ => None,
            })
        })
        .unwrap();
    assert_eq!(
        ledger.files_changed.as_deref(),
        Some(&["a.txt".to_string(), "where.txt".to_string()][..])
    );
}

/// Chat-only agents pay nothing: no write-capable tool, no track, and the
/// turn's cwd is the project itself.
#[tokio::test]
async fn no_write_tool_means_no_track() {
    let (_td, project) = git_repo();
    let cwd = crate::tools::fs_policy::SessionCwd::new(project.clone());
    let runner = TaskRunner::new_stub_echo()
        .with_session_cwd(cwd.clone(), vec![project.to_string_lossy().into_owned()]);
    let mut spec = user_turn("hi", "t-chat", None);
    spec.cwd = Some(project.clone());
    let _ = runner.run_sync(spec).await;
    assert!(!project.join(".worktrees").exists());
    assert_eq!(cwd.for_turn("t-chat"), project);
}

/// The model names the project's absolute path as `bash`'s `cwd` — the one
/// deliberate way out of the track that the eager cwd rebind does not
/// cover. The write must still land in the track (so the diff counts it
/// and promote carries it), never straight into the project.
#[tokio::test]
async fn explicit_project_cwd_in_bash_stays_in_the_track() {
    let (_td, project) = git_repo();
    let cwd = crate::tools::fs_policy::SessionCwd::new(project.clone());
    let bash: Arc<dyn crate::tools::ToolExecutor> = Arc::new(crate::tools::bash::BashTool::new(
        project.clone(),
        cwd.clone(),
    ));
    let mut call = tool_call_response("c1", "pwd > where.txt");
    call.tool_calls[0].input["cwd"] =
        serde_json::Value::String(project.to_string_lossy().into_owned());
    let runner = TaskRunner::with_llm(Arc::new(SequenceLlm::new(vec![
        call,
        end_turn_response("done"),
    ])))
    .with_tools(vec![bash])
    .with_tools_policy(vec![mur_common::agent::ToolRule {
        pattern: "bash".into(),
        policy: mur_common::agent::ToolPolicy::Allow,
        risk: None,
    }])
    .with_sandbox_enforcing(true)
    .with_pending_approvals(empty_pending_approvals())
    .with_notifier(tokio::sync::mpsc::channel(16).0)
    .with_session_cwd(cwd.clone(), vec![project.to_string_lossy().into_owned()]);
    let mut spec = loop_spec("edit");
    spec.task_id = Some("turn-cwd".into());
    spec.cwd = Some(project.clone());

    let TaskOutcome::Completed(task) = runner.run_sync(spec).await else {
        panic!("turn did not complete");
    };
    let where_ = std::fs::read_to_string(project.join("where.txt")).unwrap();
    assert!(
        where_.contains("/.worktrees/turn-"),
        "explicit project cwd was redirected into the track: {where_}"
    );
    let text = task
        .messages
        .last()
        .map(|m| {
            m.parts
                .iter()
                .filter_map(|p| match p {
                    MessagePart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>()
        })
        .unwrap_or_default();
    assert!(
        text.contains("~ changed    1 file(s)\n"),
        "counted by the diff: {text}"
    );
    assert!(!text.contains("tool-reported"), "{text}");
}
