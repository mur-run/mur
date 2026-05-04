//! Track C2 — Telegram bridge inbound loop tests.
//!
//! Covers M-c2.2.1 .. M-c2.2.4: teloxide dep imports cleanly, the
//! `TelegramInboundLoop` skeleton constructs, and `tick_once()` honours
//! dedupe / privacy / 5xx-pinning / signed-forward semantics.

use mur_agent_runtime::bridge::ack::AckTracker;
use mur_agent_runtime::bridge::dedupe::DedupeStore;
use mur_agent_runtime::bridge::telegram::inbound::{InboundDeps, TelegramInboundLoop};
use mur_agent_runtime::bridge::telegram::mock::{MockBot, MockUpdate, MockUserAgent};
use mur_common::bridge::{PrivacyMode, TelegramConfig};
use mur_common::identity::AgentIdentity;

#[test]
fn teloxide_imports_compile() {
    // Smoke check: the symbol resolves and the crate links. We don't
    // actually construct a Bot here (that requires a token + tokio
    // runtime).
    let _ = std::any::type_name::<teloxide::Bot>();
}

#[test]
fn loop_can_be_constructed() {
    let bot = MockBot::default();
    let l = TelegramInboundLoop::stub_new(bot);
    assert_eq!(l.offset(), 0);
}

#[tokio::test]
async fn dedupe_skips_repeat_update() {
    let bot = MockBot::default();
    bot.queued_updates.lock().unwrap().push(MockUpdate {
        id: 1,
        chat_id: 100,
        is_private: true,
        text: Some("hi".into()),
        voice_file_id: None,
        document_file_id: None,
        photo_file_id: None,
        caption: None,
        file_size: None,
    });
    bot.queued_updates.lock().unwrap().push(MockUpdate {
        id: 1,
        chat_id: 100,
        is_private: true,
        text: Some("hi".into()),
        voice_file_id: None,
        document_file_id: None,
        photo_file_id: None,
        caption: None,
        file_size: None,
    });
    let deps = test_deps();
    let mut l = TelegramInboundLoop::new(bot, deps);
    let n = l.tick_once().await.unwrap();
    assert_eq!(n, 1, "second update with same id was deduped");
}

#[tokio::test]
async fn group_skipped_in_dm_only_mode() {
    let bot = MockBot::default();
    bot.queued_updates.lock().unwrap().push(MockUpdate {
        id: 5,
        chat_id: -1001,
        is_private: false,
        text: Some("group msg".into()),
        voice_file_id: None,
        document_file_id: None,
        photo_file_id: None,
        caption: None,
        file_size: None,
    });
    let deps = test_deps();
    let mut l = TelegramInboundLoop::new(bot, deps);
    let n = l.tick_once().await.unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn allow_groups_passes_listed_chat() {
    let bot = MockBot::default();
    bot.queued_updates.lock().unwrap().push(MockUpdate {
        id: 8,
        chat_id: -1001,
        is_private: false,
        text: Some("from listed group".into()),
        voice_file_id: None,
        document_file_id: None,
        photo_file_id: None,
        caption: None,
        file_size: None,
    });
    let mut deps = test_deps();
    deps.config.privacy_mode = PrivacyMode::AllowGroups;
    deps.config.allow_groups = vec![-1001];
    let mut l = TelegramInboundLoop::new(bot, deps);
    let n = l.tick_once().await.unwrap();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn offset_pinned_on_5xx() {
    let bot = MockBot::default();
    bot.queued_updates.lock().unwrap().push(MockUpdate {
        id: 7,
        chat_id: 100,
        is_private: true,
        text: Some("x".into()),
        voice_file_id: None,
        document_file_id: None,
        photo_file_id: None,
        caption: None,
        file_size: None,
    });
    let mut deps = test_deps();
    deps.always_5xx = true;
    let mut l = TelegramInboundLoop::new(bot, deps);
    let _ = l.tick_once().await;
    assert_eq!(l.offset(), 0, "offset must not advance on 5xx");
}

#[tokio::test]
async fn signed_envelope_reaches_user_agent() {
    let bot = MockBot::default();
    bot.queued_updates.lock().unwrap().push(MockUpdate {
        id: 11,
        chat_id: 100,
        is_private: true,
        text: Some("hello".into()),
        voice_file_id: None,
        document_file_id: None,
        photo_file_id: None,
        caption: None,
        file_size: None,
    });
    let ua = MockUserAgent::default();
    let mut deps = test_deps();
    deps.user_agent = Some(ua.handle());

    let mut l = TelegramInboundLoop::new(bot, deps);
    let n = l.tick_once().await.unwrap();
    assert_eq!(n, 1);

    let received = ua.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].method, "message/send");
    assert!(
        received[0].verified,
        "envelope must verify against the bridge pubkey it embedded"
    );
}

fn test_deps() -> InboundDeps {
    InboundDeps {
        config: TelegramConfig {
            bot_username: "B".into(),
            bot_token_keychain_account: "x".into(),
            chat_id: 100,
            privacy_mode: PrivacyMode::DmOnly,
            allow_groups: vec![],
            e2e_disclosure_acked_at: None,
        },
        dedupe: DedupeStore::in_memory().unwrap(),
        ack: AckTracker::<i64>::new(0),
        identity: AgentIdentity::generate(),
        key_version: 0,
        always_5xx: false,
        user_agent: None,
        agent_home: std::env::temp_dir().join(format!("mur-c2-test-{}", std::process::id())),
        whisper_stub: None,
    }
}
