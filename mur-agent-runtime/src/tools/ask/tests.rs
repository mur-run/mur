//! Channel consult D (R1): the asker's runtime writes the `Ask`, and the tool
//! round-trips through a real `channel/consult` over a unix socket.

use super::*;
use crate::protocol::a2a_server::{Dispatcher, RequestContext};
use crate::protocol::methods::channel_consult::ChannelConsultHandler;
use crate::task_runner::TaskRunner;
use tempfile::TempDir;

const FIXTURE: &str = include_str!("../../../../mur-common/tests/fixtures/minimal_profile.yaml");

struct World {
    tmp: TempDir,
    pm: Arc<AgentIdentity>,
    writer: Arc<AgentIdentity>,
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
        Self {
            tmp,
            pm: Arc::new(pm),
            writer: Arc::new(writer),
        }
    }

    fn home(&self) -> &Path {
        self.tmp.path()
    }

    fn tool(&self, sends_to: &[&str]) -> AskTool {
        AskTool {
            mur_home: self.home().to_path_buf(),
            agent_name: "pm".into(),
            identity: self.pm.clone(),
            key_version: 0,
            consult: Default::default(),
            sends_to: sends_to.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn svc(&self) -> ChannelService {
        ChannelService::open(self.home()).unwrap()
    }
}

// R1 · Asker's runtime writes the Ask.
#[test]
fn askers_runtime_writes_the_ask() {
    let w = World::new();
    let (channel, nonce) = w
        .tool(&["*"])
        .append_ask("tech-writer", "where is X?")
        .unwrap();
    let events = w.svc().load_events(&channel).unwrap();
    let ask = events
        .iter()
        .find(|e| e.kind == EventKind::Ask)
        .expect("an Ask was appended");
    assert_eq!(ask.actor, ChannelActor::Agent { id: "pm".into() });
    assert_eq!(ask.idempotency_key.as_deref(), Some(nonce.as_str()));
    let p: AskPayload = serde_json::from_value(ask.payload.clone()).unwrap();
    assert_eq!(p.to, "tech-writer");
    assert_eq!(p.nonce, nonce);
    assert!(p.expires_at_ms > chrono::Utc::now().timestamp_millis());
    // Signed with pm's own key, verified with enforcement on.
    assert!(mur_channel::verify::verify_event(
        w.home(),
        &channel,
        ask,
        true
    ));
}

#[test]
fn every_ask_gets_a_fresh_nonce_in_one_remembered_channel() {
    let w = World::new();
    let t = w.tool(&["*"]);
    let (c1, n1) = t.append_ask("tech-writer", "a").unwrap();
    let (c2, n2) = t.append_ask("tech-writer", "b").unwrap();
    assert_eq!(c1, c2);
    assert_ne!(n1, n2);
    assert_eq!(n1.len(), NONCE_BYTES * 2);
}

#[tokio::test]
async fn a_peer_outside_sends_to_is_refused_before_anything_is_written() {
    let w = World::new();
    let err = w
        .tool(&["reviewer"])
        .execute(json!({ "to": "tech-writer", "question": "q" }))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::NotAuthorized(_)), "{err:?}");
    assert!(!w.home().join("agents/pm").join(ASK_CHANNEL_FILE).exists());
}

#[tokio::test]
async fn a_peer_that_is_not_running_posts_no_ask() {
    let w = World::new();
    let err = w
        .tool(&["*"])
        .execute(json!({ "to": "tech-writer", "question": "q" }))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not running"), "{err}");
    assert!(!w.home().join("agents/pm").join(ASK_CHANNEL_FILE).exists());
}

#[tokio::test]
async fn an_agent_cannot_ask_itself() {
    let w = World::new();
    let err = w
        .tool(&["*"])
        .execute(json!({ "to": "pm", "question": "q" }))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)), "{err:?}");
}

/// D2 end to end: `ask` posts the Ask, dials a live `channel/consult` on a
/// unix socket, and returns the answer; the channel holds both events.
#[cfg(unix)]
#[tokio::test]
async fn ask_round_trips_through_channel_consult() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let w = World::new();
    let runner = TaskRunner::with_llm(Arc::new(crate::llm::stub::SequenceLlm::new(vec![
        crate::llm::LlmResponse {
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            text: "the answer".into(),
            input_tokens: 1,
            output_tokens: 1,
            model: "test".into(),
            tool_calls: vec![],
            stop_reason: crate::llm::StopReason::EndTurn,
            registry_key: None,
        },
    ])));
    let mut d = Dispatcher::new();
    d.register(
        mur_common::build::CHANNEL_CONSULT_METHOD,
        Box::new(
            ChannelConsultHandler::new(
                Arc::new(runner),
                w.writer.clone(),
                "tech-writer".into(),
                0,
                w.home().to_path_buf(),
                &w.home().join("agents/tech-writer"),
            )
            .with_platform(None),
        ),
    );
    let d = Arc::new(d);

    // Short socket path: sun_path is ~104 bytes on macOS.
    let sock_dir = tempfile::Builder::new()
        .prefix("mc")
        .tempdir_in("/tmp")
        .unwrap();
    let sock = sock_dir.path().join("a.sock");
    let listener = tokio::net::UnixListener::bind(&sock).unwrap();
    {
        let d = d.clone();
        tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            let (r, mut wr) = s.into_split();
            let mut line = String::new();
            BufReader::new(r).read_line(&mut line).await.unwrap();
            let req = serde_json::from_str(&line).unwrap();
            let resp = d.dispatch(req, &RequestContext::none()).await.unwrap();
            let out = serde_json::to_string(&resp).unwrap();
            wr.write_all(format!("{out}\n").as_bytes()).await.unwrap();
        });
    }
    let lock = json!({
        "schema": 1, "uuid": "u", "name": "tech-writer", "pid": 1, "ppid": 1,
        "started_at": "now", "binary_version": "t",
        "transports": { "stdio": false, "unix_socket": sock.display().to_string() },
        "card_digest": "d", "capabilities": [],
        "proto_version": mur_common::build::A2A_PROTO_VERSION,
    });
    std::fs::write(
        w.home().join("agents/tech-writer/running.lock"),
        lock.to_string(),
    )
    .unwrap();

    let out = w
        .tool(&["tech-writer"])
        .execute(json!({ "to": "tech-writer", "question": "what is X?" }))
        .await
        .unwrap();
    assert!(out.text.contains("the answer"), "{}", out.text);

    let channel =
        std::fs::read_to_string(w.home().join("agents/pm").join(ASK_CHANNEL_FILE)).unwrap();
    let kinds: Vec<_> = w
        .svc()
        .load_events(channel.trim())
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(kinds, vec![EventKind::Ask, EventKind::AskReply]);
}

#[test]
fn an_old_peer_is_named_with_the_restart_hint() {
    let w = World::new();
    let lock = json!({
        "schema": 1, "uuid": "u", "name": "tech-writer", "pid": 1, "ppid": 1,
        "started_at": "now", "binary_version": "t",
        "transports": { "stdio": false, "unix_socket": "/nowhere" },
        "card_digest": "d", "capabilities": [], "proto_version": 4,
    });
    std::fs::write(
        w.home().join("agents/tech-writer/running.lock"),
        lock.to_string(),
    )
    .unwrap();
    let err = crate::protocol::peer_dial::peer_socket(
        w.home(),
        "tech-writer",
        mur_common::build::CHANNEL_CONSULT_METHOD,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("mur agent restart tech-writer"),
        "{err}"
    );
}
