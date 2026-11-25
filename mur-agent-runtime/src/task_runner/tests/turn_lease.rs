//! #2564 review on #2570: a channel turn holds its conversation's lease
//! (`mur_channel::turn_lease`) from admission until after its history is
//! written, however long the model takes, so `mur channel delete` refuses
//! for exactly that span. A real channel, a real runner; the model stub
//! checks the lease mid-turn.

use super::*;
use crate::protocol::methods::message_send::ChannelContext;
use mur_channel::ChannelService;
use mur_channel::service::ChannelOwner;
use mur_channel::turn_lease::{agent_history_dir, try_exclusive};
use mur_common::channel::{ChannelActor, EventKind};

const AGENT: &str = "smith";

/// Records whether a delete could take the lease while the model "runs".
struct Probe {
    dir: std::path::PathBuf,
    ch: String,
    deletable_mid_turn: Arc<Mutex<Vec<bool>>>,
}

#[async_trait::async_trait]
impl crate::llm::LlmClient for Probe {
    async fn generate(
        &self,
        _: crate::llm::LlmRequest,
    ) -> Result<crate::llm::LlmResponse, crate::llm::LlmError> {
        let free = try_exclusive(&self.dir, &self.ch).unwrap().is_some();
        self.deletable_mid_turn.lock().unwrap().push(free);
        Ok(end_turn_response("ok"))
    }
    fn model_name(&self) -> &str {
        "turn-lease-probe"
    }
}

#[tokio::test]
async fn a_channel_turn_holds_the_lease_until_its_history_is_written() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let agent_home = home.join("agents").join(AGENT);
    mur_common::identity::AgentIdentity::generate()
        .save(&agent_home)
        .unwrap();
    let svc = ChannelService::open(home).unwrap();
    let ch = svc
        .create_for_agent(AGENT, ChannelOwner::Legacy)
        .unwrap()
        .id;
    let seq = svc
        .append_message(
            &ch,
            ChannelActor::local_human(),
            EventKind::Message,
            "hi",
            None,
        )
        .unwrap()
        .seq;
    let conv_dir = agent_home.join("conversations");
    let lease_dir = agent_history_dir(&agent_home);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let runner = TaskRunner::with_llm(Arc::new(Probe {
        dir: lease_dir.clone(),
        ch: ch.clone(),
        deletable_mid_turn: seen.clone(),
    }))
    .with_agent_name(AGENT)
    .with_channel_conversations(home.to_path_buf(), Some(conv_dir), 4);

    let spec = TaskSpec {
        channel: Some(ChannelContext {
            channel_id: ch.clone(),
            channel_seq: seq,
        }),
        ..user_turn("hi", &format!("task-{}", uuid::Uuid::now_v7()), None)
    };
    let outcome = runner.run_sync(spec).await;

    assert!(matches!(outcome, TaskOutcome::Completed(_)), "{outcome:?}");
    let seen = seen.lock().unwrap();
    assert!(!seen.is_empty(), "model never called");
    assert!(
        seen.iter().all(|free| !free),
        "a delete could run mid-turn: {seen:?}"
    );
    assert!(
        lease_dir.join(format!("{ch}.json")).exists(),
        "history must be written by the turn under test"
    );
    assert!(
        try_exclusive(&lease_dir, &ch).unwrap().is_some(),
        "lease must end with the turn"
    );
}
