//! Pipeline tests for the C7 Slack bridge inbound loop.

use mur_agent_runtime::bridge::ack::AckTracker;
use mur_agent_runtime::bridge::dedupe::DedupeStore;
use mur_agent_runtime::bridge::slack::inbound::{
    InboundDeps, SlackBotLike, SlackEnvelope, SlackEvent, SlackEventPayload, SlackInboundLoop,
};
use mur_agent_runtime::bridge::slack::mock::{MockSlackBot, MockUserAgentHandle};
use mur_common::bridge::{SlackConfig, SlackPrivacyMode};
use mur_common::identity::AgentIdentity;
use tempfile::TempDir;

// ── helpers ───────────────────────────────────────────────────────────────

fn test_config(privacy: SlackPrivacyMode, allowed: Vec<String>) -> SlackConfig {
    SlackConfig {
        workspace_url: "https://test.slack.com".into(),
        bot_token_keychain_account: "mur_slack_bot_test".into(),
        app_token_keychain_account: "mur_slack_app_test".into(),
        privacy_mode: privacy,
        allowed_channels: allowed,
    }
}

fn mention_envelope(channel: &str, ts: &str, text: &str) -> SlackEnvelope {
    SlackEnvelope {
        envelope_id: format!("Ev_{ts}"),
        kind: "events_api".into(),
        payload: Some(SlackEventPayload {
            event: SlackEvent {
                kind: "app_mention".into(),
                user: Some("U_SENDER".into()),
                text: Some(text.into()),
                ts: ts.into(),
                channel: channel.into(),
                channel_type: None,
                thread_ts: None,
            },
        }),
    }
}

fn dm_envelope(channel: &str, ts: &str, text: &str) -> SlackEnvelope {
    SlackEnvelope {
        envelope_id: format!("Ev_{ts}"),
        kind: "events_api".into(),
        payload: Some(SlackEventPayload {
            event: SlackEvent {
                kind: "message".into(),
                user: Some("U_SENDER".into()),
                text: Some(text.into()),
                ts: ts.into(),
                channel: channel.into(),
                channel_type: Some("im".into()),
                thread_ts: None,
            },
        }),
    }
}

fn make_deps(privacy: SlackPrivacyMode, allowed: Vec<String>, dir: &TempDir) -> InboundDeps {
    InboundDeps {
        config: test_config(privacy, allowed),
        dedupe: DedupeStore::in_memory().expect("in-memory dedupe"),
        ack: AckTracker::new(String::new()),
        identity: AgentIdentity::generate(),
        key_version: 1,
        always_5xx: false,
        user_agent: None,
        agent_home: dir.path().to_path_buf(),
    }
}

// ── M-c7.2 tests ──────────────────────────────────────────────────────────

#[test]
fn stub_loop_constructs() {
    let bot = MockSlackBot::new();
    let _loop_ = SlackInboundLoop::stub_new(bot);
}

#[tokio::test]
async fn mock_bot_records_post_message() {
    let bot = MockSlackBot::new();
    bot.post_message("C123", "hello", Some("1234567890.000001"))
        .await
        .unwrap();
    let msgs = bot.sent_messages();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].channel, "C123");
    assert_eq!(msgs[0].text, "hello");
    assert_eq!(msgs[0].thread_ts.as_deref(), Some("1234567890.000001"));
}

#[tokio::test]
async fn mock_bot_auth_test() {
    let bot = MockSlackBot::new();
    let uid = bot.auth_test().await.unwrap();
    assert_eq!(uid, "U_BOT_TEST");
}

#[test]
fn mock_user_agent_handle_forward() {
    let handle = MockUserAgentHandle::ok("pong");
    let payload = serde_json::json!({"text": "ping"});
    let (status, reply) = handle.forward(payload.clone());
    assert_eq!(status, 200);
    assert_eq!(reply, "pong");
    let received = handle.received.lock().unwrap();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0], payload);
}

#[test]
fn mock_user_agent_server_error() {
    let handle = MockUserAgentHandle::server_error();
    let (status, reply) = handle.forward(serde_json::json!({}));
    assert_eq!(status, 500);
    assert_eq!(reply, "");
}

// ── M-c7.3 tests ──────────────────────────────────────────────────────────

#[tokio::test]
async fn dm_only_mode_drops_channel_mention() {
    let dir = TempDir::new().unwrap();
    let bot = MockSlackBot::new();
    let deps = make_deps(SlackPrivacyMode::DmOnly, vec![], &dir);
    let mut loop_ = SlackInboundLoop::new(bot, deps);
    let env = mention_envelope("C_CHANNEL", "1000000001.000001", "<@U_BOT> help");
    let result = loop_.tick_once(env).await.unwrap();
    assert!(!result.forwarded, "DmOnly should drop channel mentions");
    assert_eq!(loop_.bot.sent_messages().len(), 0);
}

#[tokio::test]
async fn dm_allowed_in_dm_only_mode() {
    let dir = TempDir::new().unwrap();
    let bot = MockSlackBot::new();
    let mut deps = make_deps(SlackPrivacyMode::DmOnly, vec![], &dir);
    deps.user_agent = Some(MockUserAgentHandle::ok("reply text"));
    let mut loop_ = SlackInboundLoop::new(bot, deps);
    let env = dm_envelope("D_DM", "1000000002.000001", "hello");
    let result = loop_.tick_once(env).await.unwrap();
    assert!(result.forwarded, "DM should pass DmOnly gate");
}

#[tokio::test]
async fn allowed_channels_gate_drops_unlisted_channel() {
    let dir = TempDir::new().unwrap();
    let bot = MockSlackBot::new();
    let deps = make_deps(
        SlackPrivacyMode::DmAndMentions,
        vec!["C_ALLOWED".into()],
        &dir,
    );
    let mut loop_ = SlackInboundLoop::new(bot, deps);
    let env = mention_envelope("C_OTHER", "1000000003.000001", "<@U_BOT> help");
    let result = loop_.tick_once(env).await.unwrap();
    assert!(
        !result.forwarded,
        "channel not in allowlist should be dropped"
    );
}

#[tokio::test]
async fn duplicate_event_skipped() {
    let dir = TempDir::new().unwrap();
    let bot = MockSlackBot::new();
    let mut deps = make_deps(SlackPrivacyMode::DmAndMentions, vec![], &dir);
    deps.user_agent = Some(MockUserAgentHandle::ok("reply"));
    let mut loop_ = SlackInboundLoop::new(bot, deps);
    let env = mention_envelope("C_CHAN", "1000000004.000001", "<@U_BOT> hello");
    let r1 = loop_.tick_once(env.clone()).await.unwrap();
    let r2 = loop_.tick_once(env).await.unwrap();
    assert!(r1.forwarded, "first delivery should forward");
    assert!(!r2.forwarded, "duplicate should be skipped");
}

// ── M-c7.4 tests ──────────────────────────────────────────────────────────

#[tokio::test]
async fn mention_prefix_stripped_before_forward() {
    let dir = TempDir::new().unwrap();
    let mut deps = make_deps(SlackPrivacyMode::DmAndMentions, vec![], &dir);
    deps.user_agent = Some(MockUserAgentHandle::ok("response"));
    let bot = MockSlackBot::new();
    let mut loop_ = SlackInboundLoop::new(bot, deps);
    let env = mention_envelope("C_CHAN", "1000000005.000001", "<@U_BOT_ID> please help");
    loop_.tick_once(env).await.unwrap();

    let received = loop_
        .deps
        .as_ref()
        .unwrap()
        .user_agent
        .as_ref()
        .unwrap()
        .received
        .lock()
        .unwrap();
    let text = received[0]["payload"]["text"].as_str().unwrap();
    assert!(
        !text.contains("<@"),
        "bot prefix should be stripped, got: {text}"
    );
    assert!(
        text.contains("please help"),
        "text should remain, got: {text}"
    );
}

#[tokio::test]
async fn mention_sets_thread_ts_in_reply() {
    let dir = TempDir::new().unwrap();
    let mut deps = make_deps(SlackPrivacyMode::DmAndMentions, vec![], &dir);
    deps.user_agent = Some(MockUserAgentHandle::ok("I can help!"));
    let bot = MockSlackBot::new();
    let mut loop_ = SlackInboundLoop::new(bot, deps);
    let env = mention_envelope("C_CHAN", "1000000006.000001", "<@U_BOT> question");
    loop_.tick_once(env).await.unwrap();
    let msgs = loop_.bot.sent_messages();
    assert_eq!(msgs.len(), 1);
    assert_eq!(
        msgs[0].thread_ts.as_deref(),
        Some("1000000006.000001"),
        "mention should reply in-thread"
    );
}

#[tokio::test]
async fn dm_does_not_set_thread_ts() {
    let dir = TempDir::new().unwrap();
    let mut deps = make_deps(SlackPrivacyMode::DmAndMentions, vec![], &dir);
    deps.user_agent = Some(MockUserAgentHandle::ok("reply"));
    let bot = MockSlackBot::new();
    let mut loop_ = SlackInboundLoop::new(bot, deps);
    let env = dm_envelope("D_DM", "1000000007.000001", "hello");
    loop_.tick_once(env).await.unwrap();
    let msgs = loop_.bot.sent_messages();
    assert_eq!(msgs.len(), 1);
    assert!(msgs[0].thread_ts.is_none(), "DM should not set thread_ts");
}

#[tokio::test]
async fn a2a_5xx_does_not_advance_ack() {
    let dir = TempDir::new().unwrap();
    let mut deps = make_deps(SlackPrivacyMode::DmAndMentions, vec![], &dir);
    deps.user_agent = Some(MockUserAgentHandle::server_error());
    let bot = MockSlackBot::new();
    let mut loop_ = SlackInboundLoop::new(bot, deps);
    let env = mention_envelope("C_CHAN", "1000000008.000001", "<@U_BOT> hi");
    loop_.tick_once(env).await.unwrap();
    let committed = loop_.deps.as_ref().unwrap().ack.committed_offset();
    assert!(
        committed.is_empty(),
        "AckTracker should not advance on 5xx, got: {committed}"
    );
}

#[tokio::test]
async fn envelope_signed_correctly() {
    let dir = TempDir::new().unwrap();
    let mut deps = make_deps(SlackPrivacyMode::DmAndMentions, vec![], &dir);
    let pubkey = deps.identity.public_key_multibase();
    deps.user_agent = Some(MockUserAgentHandle::ok("ok"));
    let bot = MockSlackBot::new();
    let mut loop_ = SlackInboundLoop::new(bot, deps);
    let env = dm_envelope("D_DM", "1000000009.000001", "test");
    loop_.tick_once(env).await.unwrap();

    let received = loop_
        .deps
        .as_ref()
        .unwrap()
        .user_agent
        .as_ref()
        .unwrap()
        .received
        .lock()
        .unwrap();
    assert!(
        received[0].get("signature").is_some(),
        "forwarded payload missing signature"
    );
    let env_pubkey = received[0]["bridge_pubkey_multibase"]
        .as_str()
        .unwrap_or("");
    assert_eq!(env_pubkey, pubkey, "pubkey mismatch");
}
