use super::*;

/// The user's actual failure mode, at the last mile before the LLM: a
/// saved memory must appear in the system prompt the model receives.
/// Tested here rather than only in the injector because the injector
/// returning the right string proves nothing if the addendum never
/// reaches `assemble_system_prompt`'s output.
#[test]
fn saved_memory_reaches_the_system_prompt() {
    use mur_common::skill::loader::{LoadedSkill, SkillScope};
    use mur_common::skill::note::{NoteSpec, note_manifest};
    use mur_common::skill::types::TrustLevel;

    let note = LoadedSkill {
        name: "reply-in-zh-tw".into(),
        manifest: note_manifest(&NoteSpec {
            name: "reply-in-zh-tw",
            description: "reply in Traditional Chinese",
            body: "ALWAYS-REPLY-IN-ZH-TW",
            kind: mur_common::skill::lifecycle::NoteKind::Rule,
            publisher: "agent:mur",
        }),
        // What the loader really assigns an agent-written note: it is
        // never in the trust store, so it loads Sandboxed.
        trust: TrustLevel::Sandboxed,
        scope: SkillScope::Agent,
        content_hash: String::new(),
        dir: std::path::PathBuf::new(),
    };
    let runner = TaskRunner::new_stub_echo()
        .with_system_prompt(Some("BASE PROMPT".into()))
        .with_skills(Arc::new(RuntimeSkills::build(vec![note])));

    let (sys, _fired) = runner.assemble_system_prompt(None, "hello", None, None);
    assert!(
        sys.contains("ALWAYS-REPLY-IN-ZH-TW"),
        "the saved memory must reach the model's system prompt; got:\n{sys}"
    );
    assert!(
        sys.starts_with("BASE PROMPT"),
        "the agent's own prompt still leads"
    );
}

/// The path lives in the system prompt, read from the runtime's own
/// session cwd — so it survives any amount of history trimming.
#[tokio::test]
async fn working_directory_reaches_the_system_prompt_every_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let root = mur_track::turn::canonicalize(tmp.path()).unwrap();
    // The project differs from the home, so the path can only reach the
    // prompt through this conversation's cwd, never the home fallback.
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let cwd = crate::tools::fs_policy::SessionCwd::new(root.clone());
    let runner = TaskRunner::new_stub_echo()
        .with_system_prompt(Some("BASE".into()))
        .with_session_cwd(cwd, vec![root.to_string_lossy().into_owned()]);
    // Far more turns than any history cap, all in one conversation; only
    // the first names the directory.
    let mut ctx: Option<String> = None;
    for i in 0..60 {
        let id = format!("t{i}");
        let mut spec = user_turn("hi", &id, ctx.as_deref());
        spec.cwd = (i == 0).then(|| project.clone());
        let _ = runner.run_sync(spec).await;
        ctx = Some(id);
    }
    let (sys, _) = runner.assemble_system_prompt(ctx.as_deref(), "hello", None, None);
    assert!(sys.contains("## Working directory"), "{sys}");
    assert!(
        sys.contains(&project.to_string_lossy().into_owned()),
        "{sys}"
    );
    assert!(
        !sys.contains("never write them into the current working directory"),
        "the old wording that steered project files into ~/.mur is gone"
    );
}

#[tokio::test]
async fn turn_cwd_moves_the_session_cwd_only_within_entitlements() {
    let tmp = tempfile::tempdir().unwrap();
    let root = mur_track::turn::canonicalize(tmp.path()).unwrap();
    let project = root.join("project");
    let outside = root.join("outside");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let cwd = crate::tools::fs_policy::SessionCwd::new(root.clone());
    let runner = TaskRunner::new_stub_echo()
        .with_session_cwd(cwd.clone(), vec![project.to_string_lossy().into_owned()]);

    // One conversation: t1 → t2 → t3.
    let mut spec = user_turn("hi", "t1", None);
    spec.cwd = Some(project.clone());
    let _ = runner.run_sync(spec).await;
    assert_eq!(cwd.for_turn("t1"), project, "entitled cwd is adopted");

    let mut spec = user_turn("hi", "t2", Some("t1"));
    spec.cwd = Some(outside);
    let _ = runner.run_sync(spec).await;
    assert_eq!(
        cwd.for_turn("t2"),
        project,
        "unentitled cwd is refused, conversation cwd kept"
    );

    let _ = runner.run_sync(user_turn("hi", "t3", Some("t2"))).await;
    assert_eq!(
        cwd.for_turn("t3"),
        project,
        "absent cwd leaves the conversation cwd alone"
    );
}

/// Dogfood bug: two murmur sessions on one agent shared ONE cwd, so the
/// session that spoke last dragged every other session's tools into its
/// directory ("my gateway session was suddenly working in mur/").
#[tokio::test]
async fn each_session_keeps_its_own_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let root = mur_track::turn::canonicalize(tmp.path()).unwrap();
    let gateway = root.join("gateway");
    let mur = root.join("mur");
    std::fs::create_dir_all(&gateway).unwrap();
    std::fs::create_dir_all(&mur).unwrap();
    let cwd = crate::tools::fs_policy::SessionCwd::new(root.clone());
    let runner = TaskRunner::new_stub_echo()
        .with_session_cwd(cwd.clone(), vec![root.to_string_lossy().into_owned()]);
    let turn = |id: &str, ctx: Option<&str>, dir: Option<&std::path::Path>| {
        let mut spec = user_turn("hi", id, ctx);
        spec.cwd = dir.map(std::path::Path::to_path_buf);
        spec
    };
    // What a tool call inside turn `id` resolves relative paths against.
    let seen_by = |id: &str| {
        let cwd = cwd.clone();
        crate::tools::bash_jobs::CURRENT_TASK_ID.scope(id.to_string(), async move { cwd.current() })
    };

    let _ = runner.run_sync(turn("a1", None, Some(&gateway))).await;
    let _ = runner.run_sync(turn("b1", None, Some(&mur))).await;
    // Session A carries on without restating its cwd (Hub, `mur agent send`).
    let _ = runner.run_sync(turn("a2", Some("a1"), None)).await;
    // A brand-new session that never named a directory.
    let _ = runner.run_sync(turn("c1", None, None)).await;

    assert_eq!(seen_by("a2").await, gateway, "session A kept its own cwd");
    assert_eq!(seen_by("b1").await, mur, "session B kept its own cwd");
    assert_eq!(
        seen_by("c1").await,
        root,
        "a new session starts at the agent home, not the last speaker's cwd"
    );
}

fn prompt_with_project_agents_md() -> String {
    let tmp = tempfile::tempdir().unwrap();
    let root = mur_track::turn::canonicalize(tmp.path()).unwrap();
    std::fs::create_dir(root.join(".git")).unwrap();
    std::fs::write(root.join("AGENTS.md"), "PROJECT-RULE: run cargo fmt").unwrap();
    let grant = root.to_string_lossy().into_owned();
    let gate = crate::project_instructions::ProjectInstructions::new(
        mur_common::agent::FilesystemEntitlement {
            read: vec![grant.clone()],
            ..Default::default()
        },
        crate::sandbox::launch_chain::LaunchChain::inert(),
    );
    let runner = TaskRunner::new_stub_echo()
        .with_system_prompt(Some("BASE".into()))
        .with_session_cwd(
            crate::tools::fs_policy::SessionCwd::new(root.clone()),
            vec![grant],
        )
        .with_project_instructions(gate);
    runner.assemble_system_prompt(None, "hello", None, None).0
}

/// The system prompt names the pinned block right after the path it
/// describes, and carries no `## Project instructions` heading (§3.3, §7.3).
#[test]
fn system_prompt_names_the_pinned_block() {
    let sys = prompt_with_project_agents_md();
    let wd = sys.find("## Working directory").expect("cwd line");
    let rule = sys
        .find("The first user message may begin with a `<project_instructions>` block.")
        .expect("rule paragraph");
    assert!(wd < rule, "the rule follows the working directory:\n{sys}");
    assert!(sys.contains("Precedence, highest first:"), "{sys}");
    assert!(!sys.contains("## Project instructions"), "{sys}");
}

/// File contents travel in the pinned user message, not the system prompt.
#[test]
fn system_prompt_carries_no_project_file_contents() {
    let sys = prompt_with_project_agents_md();
    assert!(!sys.contains("PROJECT-RULE: run cargo fmt"), "{sys}");
}

// ── T6: the pinned block end to end (spec §3.1, §4.2, §7.3, §7.6) ──

/// Records every request's message list as sent, typed (not `Debug`).
struct PinnedRecordingLlm {
    responses: Vec<crate::llm::LlmResponse>,
    index: std::sync::atomic::AtomicUsize,
    seen: std::sync::Mutex<Vec<Vec<crate::llm::RichMessage>>>,
}

#[async_trait::async_trait]
impl crate::llm::LlmClient for PinnedRecordingLlm {
    async fn generate(
        &self,
        req: crate::llm::LlmRequest,
    ) -> Result<crate::llm::LlmResponse, crate::llm::LlmError> {
        self.seen.lock().unwrap().push(req.messages);
        let idx = self.index.fetch_add(1, Ordering::Relaxed);
        Ok(self.responses[idx % self.responses.len()].clone())
    }
    fn model_name(&self) -> &str {
        "pinned-recording-stub"
    }
}

/// `build` tool that overwrites `AGENTS.md` with `B` when it runs — the
/// seam for "a file edited mid-turn does not change later steps" (§3.1).
struct RewriteAgentsMdTool {
    path: std::path::PathBuf,
}

#[async_trait::async_trait]
impl crate::tools::ToolExecutor for RewriteAgentsMdTool {
    fn name(&self) -> &str {
        "build"
    }
    fn def(&self) -> crate::llm::ToolDef {
        crate::llm::ToolDef {
            name: "build".into(),
            description: "test tool that rewrites AGENTS.md".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }
    async fn execute(
        &self,
        _input: serde_json::Value,
    ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
        std::fs::write(&self.path, "RULE-B").unwrap();
        Ok("rewrote".to_string().into())
    }
}

/// A repo root with `AGENTS.md` = `RULE-A`, a runner whose session cwd and
/// read grant point at it, and the recorder it talks to.
fn pinned_runner(
    responses: Vec<crate::llm::LlmResponse>,
) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    Arc<PinnedRecordingLlm>,
    Arc<TaskRunner>,
) {
    let tmp = tempfile::tempdir().unwrap();
    let root = mur_track::turn::canonicalize(tmp.path()).unwrap();
    std::fs::create_dir(root.join(".git")).unwrap();
    let agents = root.join("AGENTS.md");
    std::fs::write(&agents, "RULE-A").unwrap();
    let grant = root.to_string_lossy().into_owned();
    let gate = crate::project_instructions::ProjectInstructions::new(
        mur_common::agent::FilesystemEntitlement {
            read: vec![grant.clone()],
            ..Default::default()
        },
        crate::sandbox::launch_chain::LaunchChain::inert(),
    );
    let llm = Arc::new(PinnedRecordingLlm {
        responses,
        index: std::sync::atomic::AtomicUsize::new(0),
        seen: std::sync::Mutex::new(Vec::new()),
    });
    let runner = Arc::new(
        TaskRunner::with_llm(llm.clone())
            .with_system_prompt(Some("BASE".into()))
            .with_session_cwd(
                crate::tools::fs_policy::SessionCwd::new(root.clone()),
                vec![grant],
            )
            .with_project_instructions(gate)
            .with_tools(vec![Arc::new(RewriteAgentsMdTool { path: agents })])
            .with_tools_policy(vec![mur_common::agent::ToolRule {
                pattern: "build".into(),
                policy: mur_common::agent::ToolPolicy::Allow,
                risk: None,
            }])
            .with_pending_approvals(empty_pending_approvals())
            .with_notifier(tokio::sync::mpsc::channel(16).0)
            .with_hitl_timeout_secs(1),
    );
    (tmp, root, llm, runner)
}

fn is_pinned_block(m: &crate::llm::RichMessage) -> bool {
    matches!(m, crate::llm::RichMessage::Text { role, content }
        if role == "user" && content.starts_with("<project_instructions"))
}

/// Any non-system message carrying the block. The system prompt's rule
/// paragraph names `<project_instructions>` by design, so it is excluded.
fn mentions_block(m: &crate::llm::RichMessage) -> bool {
    let system = matches!(m, crate::llm::RichMessage::Text { role, .. } if role == "system");
    !system && format!("{m:?}").contains("<project_instructions")
}

#[tokio::test]
async fn pinned_block_is_never_stored_in_conversation_memory() {
    let (_tmp, _root, llm, runner) = pinned_runner(vec![
        build_tool_call_response("m-0"),
        end_turn_response("done"),
    ]);
    let mut spec = loop_spec("first");
    spec.context_task_id = Some("ctx-0".into());
    let TaskOutcome::Completed(task) = runner.run_sync(spec).await else {
        panic!("expected Completed");
    };
    // Guard that the block was really sent, so the assertion below means something.
    assert!(llm.seen.lock().unwrap()[0].iter().any(is_pinned_block));
    let stored = runner.stored_prior(Some(&task.id));
    assert!(!stored.is_empty(), "the turn itself is remembered");
    assert!(
        !stored.iter().any(mentions_block),
        "pinned block leaked into memory: {stored:?}"
    );
}

#[tokio::test]
async fn every_tool_loop_step_sends_exactly_one_pinned_block_at_index_1() {
    let (_tmp, _root, llm, runner) = pinned_runner(vec![
        build_tool_call_response("s-0"),
        build_tool_call_response("s-1"),
        end_turn_response("done"),
    ]);
    let TaskOutcome::Completed(_) = runner.run_sync(loop_spec("go")).await else {
        panic!("expected Completed");
    };
    let seen = llm.seen.lock().unwrap();
    assert!(seen.len() >= 3, "three steps, got {}", seen.len());
    for (step, msgs) in seen.iter().enumerate() {
        let hits: Vec<usize> = msgs
            .iter()
            .enumerate()
            .filter(|(_, m)| mentions_block(m))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(hits, vec![1], "step {step}: {msgs:?}");
        assert!(is_pinned_block(&msgs[1]), "step {step}: {msgs:?}");
    }
}

#[tokio::test]
async fn render_is_called_once_per_turn_not_per_step() {
    let (_tmp, root, llm, runner) = pinned_runner(vec![
        build_tool_call_response("r-0"),
        end_turn_response("done"),
    ]);
    let TaskOutcome::Completed(_) = runner.run_sync(loop_spec("go")).await else {
        panic!("expected Completed");
    };
    assert_eq!(
        std::fs::read_to_string(root.join("AGENTS.md")).unwrap(),
        "RULE-B"
    );
    let seen = llm.seen.lock().unwrap();
    assert!(seen.len() >= 2, "two steps, got {}", seen.len());
    for (step, msgs) in seen.iter().enumerate() {
        let block = format!("{:?}", msgs[1]);
        assert!(block.contains("RULE-A"), "step {step}: {block}");
        assert!(
            !block.contains("RULE-B"),
            "step {step} re-rendered: {block}"
        );
    }
}

#[test]
fn no_session_cwd_means_no_project_instructions_rule() {
    let runner = TaskRunner::new_stub_echo().with_system_prompt(Some("BASE".into()));
    let (sys, _) = runner.assemble_system_prompt(None, "hello", None, None);
    assert!(!sys.contains("<project_instructions>"), "{sys}");
    assert!(!sys.contains("Precedence, highest first:"), "{sys}");
}

#[test]
fn no_session_cwd_means_no_working_directory_line() {
    let runner = TaskRunner::new_stub_echo().with_system_prompt(Some("BASE".into()));
    let (sys, _) = runner.assemble_system_prompt(None, "hello", None, None);
    assert!(!sys.contains("## Working directory"));
}

#[test]
fn secret_names_reach_the_system_prompt_and_values_do_not() {
    let vault = Arc::new(crate::secrets::SecretVault::new());
    vault
        .set("GITEA_TOKEN", "d8b04a3cc632a5c8026cf5a810d36e292c603f99")
        .unwrap();
    let runner = TaskRunner::new_stub_echo()
        .with_system_prompt(Some("BASE PROMPT".into()))
        .with_secrets(vault);
    let (sys, _) = runner.assemble_system_prompt(None, "hello", None, None);
    assert!(sys.contains("$GITEA_TOKEN"), "{sys}");
    assert!(!sys.contains("d8b04a3c"), "{sys}");
}

#[test]
fn tool_output_is_masked_before_it_becomes_a_result() {
    let vault = Arc::new(crate::secrets::SecretVault::new());
    vault
        .set("GITEA_TOKEN", "d8b04a3cc632a5c8026cf5a810d36e292c603f99")
        .unwrap();
    let runner = TaskRunner::new_stub_echo().with_secrets(vault);
    assert_eq!(
        runner
            .guarded()
            .masked("got d8b04a3cc632a5c8026cf5a810d36e292c603f99 back".into()),
        "got [SECRET:GITEA_TOKEN] back"
    );
    // No vault: passthrough, no allocation surprise for the common case.
    let bare = TaskRunner::new_stub_echo();
    assert_eq!(bare.guarded().masked("x".into()), "x");
}

#[test]
fn assemble_system_prompt_appends_output_locations_rule() {
    let runner = TaskRunner::new_stub_echo().with_system_prompt(Some("BASE PROMPT".into()));
    let (sys, _fired) = runner.assemble_system_prompt(None, "hello", None, None);
    assert!(
        sys.starts_with("BASE PROMPT"),
        "keeps the agent's own prompt first"
    );
    assert!(sys.contains("Output locations"), "injects the rule heading");
    assert!(
        sys.contains("~/.mur/artifacts/"),
        "names the run-artifact dir"
    );
    assert!(
        sys.contains("mur skill install"),
        "names the register command"
    );
}

/// A delegated turn has no human on the other end: the fleet router that
/// dialled `channel/delegate` is a program waiting on a reply, not someone
/// who can answer an approval prompt. Before this it registered nothing, so
/// the gate fell through to asking and burned the whole `hitl.timeout_secs`
/// on an agent-wide notifier nobody was reading — the turn came back empty
/// with approval never named as the cause.
#[tokio::test]
async fn an_unattended_turn_is_refused_at_once_with_a_readable_reason() {
    let calls = Arc::new(AtomicU64::new(0));
    let runner = Arc::new(
        TaskRunner::new_stub_echo()
            .with_sandbox_enforcing(true)
            .with_tools(vec![Arc::new(CountingBashTool {
                calls: calls.clone(),
                ..Default::default()
            })])
            .with_tools_policy(vec![mur_common::agent::ToolRule {
                pattern: "bash".into(),
                policy: mur_common::agent::ToolPolicy::Ask,
                risk: None,
            }]),
    );
    let call = crate::llm::ToolCallResult {
        call_id: "c-1".into(),
        tool_name: "bash".into(),
        input: serde_json::json!({"command": "echo hi"}),
    };

    // Marked unattended → the readable refusal, before any prompt is sent.
    runner.mark_unattended("t-delegated").await;
    let (out, _) = runner
        .gate_response("t-delegated", std::slice::from_ref(&call))
        .await;
    let d = out.get("c-1").expect("a decision for the gated call");
    assert!(!d.allow);
    let why = d.reason.clone().unwrap_or_default();
    assert!(why.contains("bash"), "must name the tool: {why}");
    assert!(why.contains("tool-allow"), "must name the way out: {why}");
    assert_eq!(calls.load(Ordering::Relaxed), 0, "nothing may execute");

    // Negative control: an unmarked turn takes the old no-sink path, whose
    // refusal names neither the tool nor a remedy. If this ever matches the
    // assertions above, the test has stopped distinguishing the two paths.
    let (out, _) = runner
        .gate_response("t-unmarked", std::slice::from_ref(&call))
        .await;
    let why = out
        .get("c-1")
        .and_then(|d| d.reason.clone())
        .unwrap_or_default();
    assert!(!why.contains("tool-allow"), "different path: {why}");
}

/// #10 prompt half: a granted scratch dir adds the spec's line right after
/// the artifacts bullet; without one the rule is byte-for-byte unchanged.
#[test]
fn scratch_line_follows_artifacts_bullet_only_when_granted() {
    let p = std::path::Path::new("/home/u/.mur/tmp/w1");
    let with = output_locations_rule(Some(p));
    let line = "- Scratch files (temp output, intermediate data) go in `/home/u/.mur/tmp/w1` — this is also `$TMPDIR`. Never use `/tmp`: it is outside your write entitlement and write_file/edit_file will reject it.";
    let art = with.find("- Run artifacts").expect("artifacts bullet");
    let at = with.find(line).expect("scratch line present");
    assert!(at > art, "scratch line comes after the artifacts bullet");
    assert!(!with.contains("`/tmp` is not writable"));
    assert!(with.ends_with(line));

    let without = output_locations_rule(None);
    assert!(!without.contains("Scratch files"));
    assert_eq!(
        with.strip_suffix(line).unwrap().trim_end_matches('\n'),
        without
    );
}

/// The runner's own scratch dir reaches the assembled prompt.
#[test]
fn runner_scratch_dir_reaches_the_system_prompt() {
    let runner = TaskRunner::new_stub_echo().with_scratch_dir(Some("/x/tmp/a".into()));
    let (sys, _) = runner.assemble_system_prompt(None, "hi", None, None);
    assert!(sys.contains("go in `/x/tmp/a`"));
}

/// Dogfood bug (channel 01a11025): murmur sent `cwd = ~/APP/<project>` every
/// turn, the runtime refused it (granted after the sandbox sealed) and fell
/// back to the agent home with only a `tracing::warn!` — so the agent asked
/// the user "where is your project?" and nothing on screen said why. A
/// refused cwd must be stated in the reply, with the command that fixes it,
/// exactly as a refused write already is.
mod refused_cwd {
    use super::*;

    fn reply_text(outcome: &TaskOutcome) -> String {
        let TaskOutcome::Completed(task) = outcome else {
            panic!("expected Completed, got {outcome:?}")
        };
        task.messages
            .last()
            .unwrap()
            .parts
            .iter()
            .filter_map(|p| match p {
                MessagePart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn stamp(p: &std::path::Path, ago: u64) {
        std::fs::write(p, "x").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(ago))
            .unwrap();
    }

    /// `home` plays the agent dir (`~/.mur/agents/<name>`): that is where
    /// `running.lock` and `profile.yaml` live in production.
    fn fixture() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        std::path::PathBuf,
        TaskRunner,
        crate::tools::fs_policy::SessionCwd,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let home = mur_track::turn::canonicalize(tmp.path()).unwrap();
        let project = home.join("project");
        let outside = home.join("outside");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let cwd = crate::tools::fs_policy::SessionCwd::new(home.clone());
        let runner = TaskRunner::new_stub_echo()
            .with_agent_name("mur")
            .with_session_cwd(cwd.clone(), vec![project.to_string_lossy().into_owned()]);
        (tmp, home, outside, runner, cwd)
    }

    #[tokio::test]
    async fn unentitled_cwd_is_explained_in_the_reply_with_the_grant_command() {
        let (_tmp, home, outside, runner, _cwd) = fixture();
        // Profile sealed after its last edit: nothing new to pick up, so the
        // fix is a grant, not a restart alone.
        stamp(&home.join("profile.yaml"), 600);
        stamp(&home.join("running.lock"), 1);

        let mut spec = user_turn("hi", "t1", None);
        spec.cwd = Some(outside.clone());
        let text = reply_text(&runner.run_sync(spec).await);

        assert!(text.contains("[cwd]"), "{text}");
        assert!(
            text.contains(&outside.to_string_lossy().into_owned()),
            "names the refused directory: {text}"
        );
        assert!(
            text.contains(&home.to_string_lossy().into_owned()),
            "names where the turn actually ran: {text}"
        );
        assert!(text.contains("mur agent perm allow-read mur "), "{text}");
        assert!(text.contains("mur agent restart mur"), "{text}");
    }

    #[tokio::test]
    async fn cwd_granted_after_the_seal_says_restart_not_grant() {
        let (_tmp, home, outside, runner, _cwd) = fixture();
        stamp(&home.join("running.lock"), 600); // sealed ten minutes ago
        stamp(&home.join("profile.yaml"), 1); // granted a second ago

        let mut spec = user_turn("hi", "t1", None);
        spec.cwd = Some(outside.clone());
        let text = reply_text(&runner.run_sync(spec).await);

        assert!(text.contains("[cwd]"), "{text}");
        assert!(text.contains("mur agent restart mur"), "{text}");
        assert!(
            !text.contains("allow-read"),
            "the grant may already be there; do not tell the user to add it again: {text}"
        );
    }

    #[tokio::test]
    async fn missing_cwd_is_explained() {
        let (_tmp, home, _outside, runner, _cwd) = fixture();
        let gone = home.join("project").join("does-not-exist");

        let mut spec = user_turn("hi", "t1", None);
        spec.cwd = Some(gone.clone());
        let text = reply_text(&runner.run_sync(spec).await);

        assert!(text.contains("[cwd]"), "{text}");
        assert!(text.contains("does not exist"), "{text}");
        assert!(
            text.contains(&gone.to_string_lossy().into_owned()),
            "{text}"
        );
    }

    #[tokio::test]
    async fn entitled_or_absent_cwd_leaves_the_reply_alone() {
        let (_tmp, home, _outside, runner, _cwd) = fixture();

        let mut spec = user_turn("hi", "t1", None);
        spec.cwd = Some(home.join("project"));
        let text = reply_text(&runner.run_sync(spec).await);
        assert!(!text.contains("[cwd]"), "{text}");

        let text = reply_text(&runner.run_sync(user_turn("hi", "t2", Some("t1"))).await);
        assert!(
            !text.contains("[cwd]"),
            "absent cwd is not a refusal: {text}"
        );
    }

    /// The model must know too, or it hunts for the project on its own —
    /// which is exactly what happened in the field.
    #[tokio::test]
    async fn refused_cwd_reaches_the_system_prompt_of_that_turn() {
        let (_tmp, home, outside, runner, _cwd) = fixture();
        stamp(&home.join("profile.yaml"), 600);
        stamp(&home.join("running.lock"), 1);

        runner.adopt_cwd("t1", None, Some(&outside));
        let (sys, _) = runner.assemble_system_prompt(Some("t1"), "hello", None, None);

        assert!(sys.contains("## Working directory"), "{sys}");
        assert!(
            sys.contains(&outside.to_string_lossy().into_owned()),
            "names the refused directory: {sys}"
        );
        assert!(sys.contains("mur agent restart mur"), "{sys}");
        // Another turn of another conversation is untouched.
        runner.adopt_cwd("u1", None, None);
        let (sys, _) = runner.assemble_system_prompt(Some("u1"), "hello", None, None);
        assert!(!sys.contains("[cwd]"), "{sys}");
    }
}
