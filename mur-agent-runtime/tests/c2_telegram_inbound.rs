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

// ─────────────────────────────────────────────────────────────────────
// M-c2.6.3 — BridgeBeacon visible to mur agent doctor for tg bridge
// ─────────────────────────────────────────────────────────────────────

/// M-c2.6.3 — when the supervisor spawns a telegram bridge it also
/// spawns a `BridgeBeacon`; the bridge must classify as `Running`
/// (fresh `running.lock`) within a few seconds so peers consuming
/// `mur agent doctor`'s `bridges:` section see it immediately.
///
/// This is the in-process equivalent of the doctor's
/// `collect_bridge_statuses` walk — we exercise the same
/// `bridge_status_for_peer` predicate it uses, on the same
/// `running.lock` the supervisor maintains.
#[tokio::test]
async fn bridge_beacon_reports_running_within_5s() {
    use mur_agent_runtime::bridge::beacon::{BridgePeerStatus, bridge_status_for_peer};
    use mur_agent_runtime::supervisor::spawn_telegram_bridge_for_test;

    let tmp = tempfile::tempdir().unwrap();
    let handle = spawn_telegram_bridge_for_test(tmp.path()).await.unwrap();

    // Allow the spawned beacon task to schedule and the running.lock
    // to settle. The lock is written synchronously inside
    // spawn_telegram_bridge_for_test, so this sleep mostly lets the
    // tokio runtime actually start the JoinHandle — 200 ms is ample.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    assert_eq!(
        bridge_status_for_peer(tmp.path()),
        BridgePeerStatus::Running,
        "telegram bridge with fresh running.lock must read as Running"
    );

    handle.shutdown().await;
}

/// Companion test: `collect_bridge_statuses` is the helper
/// `mur agent doctor` actually calls. We can't import it from
/// `mur-agent-runtime` (it lives in `mur-core`, which depends on us),
/// so we re-implement the minimal walk here to assert end-to-end:
/// profile.yaml with `entitlements.llm.mode: off` + fresh running.lock
/// → `Running`.
#[tokio::test]
async fn telegram_bridge_dir_layout_matches_doctor_walk() {
    use mur_agent_runtime::bridge::beacon::{BridgePeerStatus, bridge_status_for_peer};
    use mur_agent_runtime::supervisor::spawn_telegram_bridge_for_test;

    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join("agents");
    let bridge_dir = agents.join("tg_bridge");
    std::fs::create_dir_all(&bridge_dir).unwrap();
    // Mirror the on-disk shape `collect_bridge_statuses` walks:
    //   <mur_home>/agents/<name>/{profile.yaml, running.lock}
    std::fs::write(
        bridge_dir.join("profile.yaml"),
        // entitlements.llm.mode = off — same shape as the bridge
        // fixture from M-c1.4.4 but parameterised for a tg bridge.
        include_str!("fixtures/bridge_profile_telegram.yaml"),
    )
    .unwrap();
    let handle = spawn_telegram_bridge_for_test(&bridge_dir).await.unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // The doctor's classification is exactly bridge_status_for_peer of
    // each `<mur_home>/agents/<name>/` dir.
    assert_eq!(
        bridge_status_for_peer(&bridge_dir),
        BridgePeerStatus::Running
    );

    handle.shutdown().await;
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
