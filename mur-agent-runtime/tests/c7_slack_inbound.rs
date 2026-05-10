//! Pipeline tests for the C7 Slack bridge inbound loop.

use mur_agent_runtime::bridge::slack::inbound::{SlackBotLike, SlackInboundLoop};
use mur_agent_runtime::bridge::slack::mock::{MockSlackBot, MockUserAgentHandle};

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
