//! Channel consult E (§D3, R2, R3, R5, R6, R9): the answerer's checks, each
//! named after its design scenario.

use super::*;
use mur_common::channel::AskPayload;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tempfile::TempDir;

const FIXTURE: &str = include_str!("../../../../../mur-common/tests/fixtures/minimal_profile.yaml");
const FAR_FUTURE: i64 = i64::MAX / 2;

/// A MUR home with signing agents `pm` (asker) and `tech-writer` (answerer),
/// and one channel.
struct World {
    tmp: TempDir,
    pm: AgentIdentity,
    writer: Arc<AgentIdentity>,
    channel: String,
}

impl World {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let pm = AgentIdentity::generate();
        let writer = AgentIdentity::generate();
        for (name, id) in [("pm", &pm), ("tech-writer", &writer)] {
            let home = tmp.path().join("agents").join(name);
            std::fs::create_dir_all(&home).unwrap();
            id.save(&home).unwrap();
            std::fs::write(home.join("profile.yaml"), FIXTURE).unwrap();
        }
        let svc = ChannelService::open(tmp.path()).unwrap();
        let channel = svc.create_for_agent("pm").unwrap().id;
        Self {
            tmp,
            pm,
            writer: Arc::new(writer),
            channel,
        }
    }

    fn home(&self) -> &std::path::Path {
        self.tmp.path()
    }

    fn svc(&self) -> ChannelService {
        ChannelService::open(self.home()).unwrap()
    }

    fn ask_payload(to: &str, nonce: &str, expires_at_ms: i64) -> Value {
        serde_json::to_value(AskPayload {
            to: to.into(),
            question: "what does the README say?".into(),
            expires_at_ms,
            nonce: nonce.into(),
        })
        .unwrap()
    }

    /// `pm` asks `to`, signed by `pm`.
    fn ask(&self, to: &str, nonce: &str, expires_at_ms: i64) {
        self.svc()
            .append_signed(
                &self.channel,
                &self.pm,
                0,
                ChannelActor::Agent { id: "pm".into() },
                EventKind::Ask,
                Self::ask_payload(to, nonce, expires_at_ms),
                Some(nonce.into()),
            )
            .unwrap();
    }

    /// An `Ask` line written straight into the log, bypassing the store: no
    /// signature, and no dedupe.
    fn raw_ask(&self, actor: &str, nonce: &str, seq: u64) {
        let ev = json!({
            "seq": seq, "ts": chrono::Utc::now(),
            "actor": { "kind": "agent", "id": actor },
            "kind": "ask",
            "payload": Self::ask_payload("tech-writer", nonce, FAR_FUTURE),
            "idempotency_key": nonce,
        });
        let path = self.svc().store().events_path(&self.channel).unwrap();
        let mut log = std::fs::read_to_string(&path).unwrap_or_default();
        log.push_str(&format!("{ev}\n"));
        std::fs::write(path, log).unwrap();
    }

    fn handler(&self, runner: TaskRunner) -> ChannelConsultHandler {
        ChannelConsultHandler::new(
            Arc::new(runner),
            self.writer.clone(),
            "tech-writer".into(),
            0,
            self.home().to_path_buf(),
            &self.home().join("agents/tech-writer"),
        )
        .with_platform(None)
    }

    fn params(&self, nonce: &str) -> Option<Value> {
        Some(json!({ "channel_id": self.channel, "nonce": nonce }))
    }

    fn replies(&self) -> Vec<AskReplyPayload> {
        self.svc()
            .load_events(&self.channel)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == EventKind::AskReply)
            .map(|e| serde_json::from_value(e.payload).unwrap())
            .collect()
    }
}

fn answers(text: &str) -> TaskRunner {
    TaskRunner::with_llm(Arc::new(crate::llm::stub::SequenceLlm::new(vec![
        end_turn(text),
    ])))
}

fn end_turn(text: &str) -> crate::llm::LlmResponse {
    crate::llm::LlmResponse {
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        text: text.into(),
        input_tokens: 1,
        output_tokens: 1,
        model: "test".into(),
        tool_calls: vec![],
        stop_reason: crate::llm::StopReason::EndTurn,
        registry_key: None,
    }
}

async fn consult(h: &ChannelConsultHandler, params: Option<Value>) -> Result<Value, HandlerError> {
    h.handle(params, &RequestContext::none()).await
}

fn denied(r: Result<Value, HandlerError>, needle: &str) {
    match r {
        Err(e) => assert!(e.to_string().contains(needle), "{needle:?} not in {e}"),
        Ok(v) => panic!("expected a refusal naming {needle:?}, got {v}"),
    }
}

#[tokio::test]
async fn a_signed_ask_addressed_here_is_answered_and_the_reply_is_signed() {
    let w = World::new();
    w.ask("tech-writer", "n1", FAR_FUTURE);
    let out = consult(&w.handler(answers("it says hi")), w.params("n1"))
        .await
        .unwrap();
    assert_eq!(out["text"], "it says hi");
    assert_eq!(out["asker"], "pm");

    let reply = w
        .svc()
        .load_events(&w.channel)
        .unwrap()
        .into_iter()
        .find(|e| e.kind == EventKind::AskReply)
        .expect("an AskReply was appended");
    assert_eq!(
        reply.actor,
        ChannelActor::Agent {
            id: "tech-writer".into()
        }
    );
    assert!(mur_channel::verify::verify_event(
        w.home(),
        &w.channel,
        &reply,
        true
    ));
    assert_eq!(
        w.replies(),
        vec![AskReplyPayload {
            nonce: "n1".into(),
            text: "it says hi".into()
        }]
    );
}

// R2 · Unsigned Ask with enforcement off.
#[tokio::test]
async fn unsigned_ask_with_enforcement_off() {
    let w = World::new();
    // SAFETY-free: the variable is only ever read; unset is the default.
    assert!(std::env::var_os("MUR_CHANNEL_REQUIRE_SIG").is_none());
    w.raw_ask("pm", "n1", 1);
    denied(
        consult(&w.handler(answers("x")), w.params("n1")).await,
        "signature",
    );
    assert!(w.replies().is_empty());
}

// R2 · Traversal in the actor id.
#[tokio::test]
async fn traversal_in_the_actor_id() {
    let w = World::new();
    // A profile planted where the traversal would land: never read.
    let planted = w.home().join("channels/x");
    std::fs::create_dir_all(&planted).unwrap();
    std::fs::write(planted.join("profile.yaml"), "not: [valid").unwrap();
    w.raw_ask("../channels/x", "n1", 1);
    denied(
        consult(&w.handler(answers("x")), w.params("n1")).await,
        "not a valid agent",
    );
}

// R2 · Two Asks share a nonce.
#[tokio::test]
async fn two_asks_share_a_nonce() {
    let w = World::new();
    w.ask("tech-writer", "n1", FAR_FUTURE);
    w.raw_ask("pm", "n1", 2);
    denied(
        consult(&w.handler(answers("x")), w.params("n1")).await,
        "2 Ask events share",
    );
}

#[tokio::test]
async fn an_unknown_nonce_is_refused() {
    let w = World::new();
    denied(
        consult(&w.handler(answers("x")), w.params("nope")).await,
        "no Ask with this nonce",
    );
}

// R3 · Ask addressed to someone else.
#[tokio::test]
async fn ask_addressed_to_someone_else() {
    let w = World::new();
    w.ask("reviewer", "n1", FAR_FUTURE);
    denied(
        consult(&w.handler(answers("x")), w.params("n1")).await,
        "addressed to \"reviewer\"",
    );
}

#[tokio::test]
async fn an_expired_ask_is_refused() {
    let w = World::new();
    w.ask("tech-writer", "n1", 1);
    denied(
        consult(&w.handler(answers("x")), w.params("n1")).await,
        "expired",
    );
}

// R3 · Second consult for the same Ask.
#[tokio::test]
async fn second_consult_for_the_same_ask() {
    let w = World::new();
    w.ask("tech-writer", "n1", FAR_FUTURE);
    let h = w.handler(answers("once"));
    consult(&h, w.params("n1")).await.unwrap();
    denied(consult(&h, w.params("n1")).await, "already answered");
    assert_eq!(w.replies().len(), 1);
}

#[tokio::test]
async fn an_unreadable_asker_profile_refuses_and_releases_the_nonce() {
    let w = World::new();
    w.ask("tech-writer", "n1", FAR_FUTURE);
    let profile = w.home().join("agents/pm/profile.yaml");
    std::fs::write(&profile, "not: [valid").unwrap();
    let h = w.handler(answers("x"));
    denied(consult(&h, w.params("n1")).await, "pm's read scope");
    // Nothing ran, so the Ask is still answerable once the profile is fixed.
    std::fs::write(&profile, FIXTURE).unwrap();
    consult(&h, w.params("n1")).await.unwrap();
}

#[tokio::test]
async fn an_unsafe_channel_id_is_invalid_params() {
    let w = World::new();
    let r = consult(
        &w.handler(answers("x")),
        Some(json!({ "channel_id": "../x", "nonce": "n1" })),
    )
    .await;
    assert!(matches!(r, Err(HandlerError::InvalidParams(_))), "{r:?}");
}

// R9 · CLI-backed answerer.
#[tokio::test]
async fn cli_backed_answerer() {
    let w = World::new();
    w.ask("tech-writer", "n1", FAR_FUTURE);
    let cli = TaskRunner::with_cli_spawn(&mur_common::cli_backend::CLAUDE);
    let r = consult(&w.handler(cli), w.params("n1")).await;
    assert!(
        matches!(r, Err(HandlerError::UnsupportedCapability(_))),
        "{r:?}"
    );
    denied(r, mur_common::cli_backend::CLAUDE.key);
    // Not recorded: a loop-backed consult can still answer it.
    consult(&w.handler(answers("later")), w.params("n1"))
        .await
        .unwrap();
}

// R6 · Linux before P2 (and any platform not yet checked).
#[tokio::test]
async fn linux_before_p2() {
    let w = World::new();
    w.ask("tech-writer", "n1", FAR_FUTURE);
    let platform = Refusal::Platform("on Linux an agent can still rewrite its own profile.yaml");
    let h = w.handler(answers("x")).with_platform(Some(platform));
    denied(consult(&h, w.params("n1")).await, "Linux");
    assert!(w.replies().is_empty());
}

#[test]
fn the_platform_guard_matches_this_build() {
    let r = checks::platform_refusal();
    if cfg!(target_os = "macos") {
        assert_eq!(r, None);
    } else {
        assert!(r.is_some(), "only macOS is cleared for consult");
    }
}

/// Records the offered tool names, emits one `bash` call, then ends.
struct BashCaller {
    offered: Arc<Mutex<Vec<Vec<String>>>>,
    step: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl crate::llm::LlmClient for BashCaller {
    async fn generate(
        &self,
        req: crate::llm::LlmRequest,
    ) -> Result<crate::llm::LlmResponse, crate::llm::LlmError> {
        self.offered
            .lock()
            .unwrap()
            .push(req.tools.iter().map(|d| d.name.clone()).collect());
        if self.step.fetch_add(1, Ordering::Relaxed) == 0 {
            let mut r = end_turn("");
            r.stop_reason = crate::llm::StopReason::ToolUse;
            r.tool_calls = vec![crate::llm::ToolCallResult {
                call_id: "c-1".into(),
                tool_name: "bash".into(),
                input: json!({ "command": "cat ~/secret" }),
            }];
            return Ok(r);
        }
        Ok(end_turn("done"))
    }
    fn model_name(&self) -> &str {
        "bash-caller"
    }
}

struct Recorder {
    name: &'static str,
    ran: Arc<AtomicBool>,
}

#[async_trait]
impl crate::tools::ToolExecutor for Recorder {
    fn name(&self) -> &str {
        self.name
    }
    fn def(&self) -> crate::llm::ToolDef {
        crate::llm::ToolDef {
            name: self.name.into(),
            description: "test tool".into(),
            input_schema: json!({ "type": "object" }),
        }
    }
    async fn execute(
        &self,
        _input: Value,
    ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
        self.ran.store(true, Ordering::Relaxed);
        Ok("ran".to_string().into())
    }
}

// R5 · Model calls bash during consult. Every tool is allowed by policy, so
// only the consult's allowlist can be what keeps bash out.
#[tokio::test]
async fn model_calls_bash_during_consult() {
    let w = World::new();
    w.ask("tech-writer", "n1", FAR_FUTURE);
    let offered = Arc::new(Mutex::new(Vec::new()));
    let bash_ran = Arc::new(AtomicBool::new(false));
    let runner = TaskRunner::with_llm(Arc::new(BashCaller {
        offered: offered.clone(),
        step: Default::default(),
    }))
    .with_tools(vec![
        Arc::new(Recorder {
            name: "bash",
            ran: bash_ran.clone(),
        }),
        Arc::new(Recorder {
            name: crate::tools::read_file::READ_FILE,
            ran: Arc::new(AtomicBool::new(false)),
        }),
    ])
    .with_tools_policy(vec![mur_common::agent::ToolRule {
        pattern: "*".into(),
        policy: mur_common::agent::ToolPolicy::Allow,
        risk: None,
        bypass_builtin_tier_floor: Default::default(),
    }])
    // The production runner always has this; without it `run_sync` takes the
    // single-shot path, which offers no tools at all.
    .with_pending_approvals(Arc::new(tokio::sync::Mutex::new(Default::default())))
    .with_notifier(tokio::sync::mpsc::channel(16).0);
    consult(&w.handler(runner), w.params("n1")).await.unwrap();

    assert!(
        !bash_ran.load(Ordering::Relaxed),
        "bash ran during a consult"
    );
    let offered = offered.lock().unwrap();
    assert!(
        offered.len() >= 2,
        "the loop ran past the bash call: {offered:?}"
    );
    for tools in offered.iter() {
        assert_eq!(tools, &vec![crate::tools::read_file::READ_FILE.to_string()]);
    }
}

#[test]
fn the_reply_key_never_collides_with_the_ask_key() {
    assert_ne!(reply_key("n1"), "n1");
}
