//! Track C2 — Telegram bridge voice-message handler tests (M-c2.3).
//!
//! Covers M-c2.3.1 .. M-c2.3.4: getFile-style download → whisper
//! transcription stub → forward as `{transcript, audio_path}`, plus
//! 30 MB cap, inbound-loop wiring, and fixture sanity check.

use mur_agent_runtime::bridge::telegram::mock::{MockBot, MockUpdate};
use mur_agent_runtime::bridge::telegram::voice::{ForwardPayload, VoiceDeps, handle_voice_update};

#[tokio::test]
async fn voice_transcript_returned() {
    let bot = MockBot::default();
    let fixture = include_bytes!("fixtures/voice_hello.ogg");
    bot.stub_file_bytes("file-1".into(), fixture.to_vec());

    let update = MockUpdate {
        id: 21,
        chat_id: 100,
        is_private: true,
        text: None,
        voice_file_id: Some("file-1".into()),
        document_file_id: None,
        photo_file_id: None,
        caption: None,
        file_size: Some(2048),
    };

    let tmp = tempfile::tempdir().unwrap();
    let deps = VoiceDeps {
        agent_home: tmp.path().to_path_buf(),
        whisper_stub: Some("hello world".into()),
    };
    let payload = handle_voice_update(&bot, &update, &deps).await.unwrap();
    match payload {
        ForwardPayload::Text {
            transcript,
            audio_path,
        } => {
            assert_eq!(transcript, "hello world");
            assert!(audio_path.exists());
        }
        _ => panic!("expected Text payload"),
    }
}

#[tokio::test]
async fn voice_oversize_bails() {
    let bot = MockBot::default();
    let update = MockUpdate {
        id: 22,
        chat_id: 100,
        is_private: true,
        text: None,
        voice_file_id: Some("big".into()),
        document_file_id: None,
        photo_file_id: None,
        caption: None,
        file_size: Some(40_000_000),
    };
    let tmp = tempfile::tempdir().unwrap();
    let deps = VoiceDeps {
        agent_home: tmp.path().into(),
        whisper_stub: Some("x".into()),
    };
    let r = handle_voice_update(&bot, &update, &deps).await;
    assert!(r.is_err());
    assert!(format!("{}", r.unwrap_err()).contains("too large"));
}

#[tokio::test]
async fn voice_routed_through_inbound_loop() {
    use mur_agent_runtime::bridge::telegram::inbound::TelegramInboundLoop;
    use mur_agent_runtime::bridge::telegram::mock::MockUserAgent;

    let bot = MockBot::default();
    let fixture = include_bytes!("fixtures/voice_hello.ogg");
    bot.stub_file_bytes("v1".into(), fixture.to_vec());
    bot.queued_updates.lock().unwrap().push(MockUpdate {
        id: 31,
        chat_id: 100,
        is_private: true,
        text: None,
        voice_file_id: Some("v1".into()),
        document_file_id: None,
        photo_file_id: None,
        caption: None,
        file_size: Some(1024),
    });

    let ua = MockUserAgent::default();
    let mut deps = TelegramInboundLoop::default_test_deps();
    deps.user_agent = Some(ua.handle());
    deps.whisper_stub = Some("transcribed".into());

    let mut loop_ = TelegramInboundLoop::new(bot, deps);
    let n = loop_.tick_once().await.unwrap();
    assert_eq!(n, 1, "voice update should be delivered once");

    let received = ua.received();
    assert_eq!(received.len(), 1);
    let body = String::from_utf8(received[0].envelope.payload.clone()).unwrap();
    assert!(
        body.contains("\"body\":\"transcribed\""),
        "expected transcript as body, got: {body}"
    );
}

#[test]
fn fixture_ogg_exists_and_nonempty() {
    let bytes = include_bytes!("fixtures/voice_hello.ogg");
    assert!(!bytes.is_empty());
    assert_eq!(&bytes[..4], b"OggS");
}
