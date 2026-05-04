//! Track C2 — Telegram bridge document / photo handler tests (M-c2.4).
//!
//! Covers M-c2.4.1 .. M-c2.4.3:
//! 1. `handle_document_update` writes a sidecar + appends a provenance
//!    entry, returning the sha256.
//! 2. The 20 MB cap rejects oversize files before the network call.
//! 3. `handle_photo_update` is symmetric for the photo branch.
//! 4. The inbound loop wires documents through with the caption as the
//!    outbound body and an `artifact_sha256` field on `params`.
//! 5. The user-agent-side B0 hook (M3.8) reads the ledger entry the
//!    bridge appended and wraps the sidecar text in
//!    `<untrusted_pdf_text>` (asserted at the hook layer — see
//!    `b0_safety_hook_wraps_pdf_text_on_user_agent` for the rationale).

use mur_agent_runtime::bridge::telegram::files::{
    FilesDeps, handle_document_update, handle_photo_update,
};
use mur_agent_runtime::bridge::telegram::mock::{MockBot, MockUpdate};

#[tokio::test]
async fn document_pipes_into_multimodal_ledger() {
    let bot = MockBot::default();
    let fixture = include_bytes!("fixtures/sample.pdf");
    bot.stub_file_bytes("doc-1".into(), fixture.to_vec());

    let update = MockUpdate {
        id: 41,
        chat_id: 100,
        is_private: true,
        text: None,
        voice_file_id: None,
        document_file_id: Some("doc-1".into()),
        photo_file_id: None,
        caption: Some("see attached".into()),
        file_size: Some(fixture.len() as u64),
    };
    let tmp = tempfile::tempdir().unwrap();
    let deps = FilesDeps {
        agent_home: tmp.path().to_path_buf(),
        mime: "application/pdf".into(),
    };
    let result = handle_document_update(&bot, &update, &deps).await.unwrap();
    assert!(result.ledger_entry.exists());
    let sha = result.sha256;
    assert_eq!(sha.len(), 64);

    // Sidecar should also exist and start with the PDF page marker so
    // B0SafetyHook tags it as `untrusted_pdf_text` (M3.8.1).
    let sidecar = tmp.path().join(format!("telemetry/inputs/{sha}.txt"));
    assert!(sidecar.exists());
    let body = std::fs::read_to_string(&sidecar).unwrap();
    assert!(body.starts_with("--- page"));
}

#[tokio::test]
async fn document_oversize_bails() {
    let bot = MockBot::default();
    let update = MockUpdate {
        id: 42,
        chat_id: 100,
        is_private: true,
        text: None,
        voice_file_id: None,
        document_file_id: Some("big".into()),
        photo_file_id: None,
        caption: None,
        file_size: Some(25_000_000),
    };
    let tmp = tempfile::tempdir().unwrap();
    let deps = FilesDeps {
        agent_home: tmp.path().into(),
        mime: "application/pdf".into(),
    };
    let r = handle_document_update(&bot, &update, &deps).await;
    assert!(r.is_err());
    assert!(format!("{}", r.unwrap_err()).contains("too large"));
}

#[tokio::test]
async fn photo_pipes_into_multimodal_ledger() {
    let bot = MockBot::default();
    let fixture = include_bytes!("fixtures/sample.png");
    bot.stub_file_bytes("ph-1".into(), fixture.to_vec());

    let update = MockUpdate {
        id: 43,
        chat_id: 100,
        is_private: true,
        text: None,
        voice_file_id: None,
        document_file_id: None,
        photo_file_id: Some("ph-1".into()),
        caption: Some("look at this".into()),
        file_size: Some(fixture.len() as u64),
    };
    let tmp = tempfile::tempdir().unwrap();
    let deps = FilesDeps {
        agent_home: tmp.path().to_path_buf(),
        mime: "image/png".into(),
    };
    let result = handle_photo_update(&bot, &update, &deps).await.unwrap();
    assert!(result.ledger_entry.exists());
    assert_eq!(result.sha256.len(), 64);
}

#[tokio::test]
async fn document_routed_through_inbound_with_caption() {
    use mur_agent_runtime::bridge::telegram::inbound::TelegramInboundLoop;
    use mur_agent_runtime::bridge::telegram::mock::MockUserAgent;

    let bot = MockBot::default();
    let fixture = include_bytes!("fixtures/sample.pdf");
    bot.stub_file_bytes("d2".into(), fixture.to_vec());
    bot.queued_updates.lock().unwrap().push(MockUpdate {
        id: 51,
        chat_id: 100,
        is_private: true,
        text: None,
        voice_file_id: None,
        document_file_id: Some("d2".into()),
        photo_file_id: None,
        caption: Some("look".into()),
        file_size: Some(fixture.len() as u64),
    });

    let ua = MockUserAgent::default();
    let tmp = tempfile::tempdir().unwrap();
    let mut deps = TelegramInboundLoop::default_test_deps();
    deps.user_agent = Some(ua.handle());
    deps.agent_home = tmp.path().to_path_buf();

    let mut loop_ = TelegramInboundLoop::new(bot, deps);
    let n = loop_.tick_once().await.unwrap();
    assert_eq!(n, 1, "document update should be delivered once");

    let received = ua.received();
    assert_eq!(received.len(), 1);
    let body_str = String::from_utf8(received[0].envelope.payload.clone()).unwrap();
    // Caption becomes the `body` field on the `message/send` params.
    assert!(
        body_str.contains("\"body\":\"look\""),
        "expected caption as body, got: {body_str}"
    );
    // The bridge attached the artifact sha256 to params (64-char hex).
    assert!(
        body_str.contains("\"artifact_sha256\":\""),
        "expected artifact_sha256 in params, got: {body_str}"
    );

    // Ledger sidecar is on disk under the staging agent_home so the
    // user-agent side can resolve `artifact_sha256` → wrapper text.
    assert!(tmp.path().join("telemetry/inputs.jsonl").exists());
}

#[tokio::test]
async fn photo_routed_through_inbound_with_caption() {
    use mur_agent_runtime::bridge::telegram::inbound::TelegramInboundLoop;
    use mur_agent_runtime::bridge::telegram::mock::MockUserAgent;

    let bot = MockBot::default();
    let fixture = include_bytes!("fixtures/sample.png");
    bot.stub_file_bytes("p2".into(), fixture.to_vec());
    bot.queued_updates.lock().unwrap().push(MockUpdate {
        id: 52,
        chat_id: 100,
        is_private: true,
        text: None,
        voice_file_id: None,
        document_file_id: None,
        photo_file_id: Some("p2".into()),
        caption: None,
        file_size: Some(fixture.len() as u64),
    });

    let ua = MockUserAgent::default();
    let tmp = tempfile::tempdir().unwrap();
    let mut deps = TelegramInboundLoop::default_test_deps();
    deps.user_agent = Some(ua.handle());
    deps.agent_home = tmp.path().to_path_buf();

    let mut loop_ = TelegramInboundLoop::new(bot, deps);
    let n = loop_.tick_once().await.unwrap();
    assert_eq!(n, 1);

    let received = ua.received();
    assert_eq!(received.len(), 1);
    let body_str = String::from_utf8(received[0].envelope.payload.clone()).unwrap();
    // Empty caption → empty body, but artifact_sha256 still attached.
    assert!(body_str.contains("\"body\":\"\""));
    assert!(body_str.contains("\"artifact_sha256\":\""));
}

/// M-c2.4.3 — assert the user-agent-side B0SafetyHook (M3.8) wraps the
/// sidecar text the bridge wrote in `<untrusted_pdf_text>`. We exercise
/// the hook directly rather than wiring a full in-process MockUserAgent
/// run loop: the bridge's responsibility ends at "ledger entry exists +
/// sidecar file written"; the hook's responsibility (and its existing
/// test coverage in `b0_untrusted_wrapper.rs`) is "given ledger + sidecar,
/// build the right `PromptPatch`". This test asserts the END-TO-END
/// invariant: bytes go in via the bridge, the untrusted wrapper comes
/// out the other side carrying the right tag.
#[tokio::test]
async fn b0_safety_hook_wraps_pdf_text_on_user_agent() {
    use mur_agent_runtime::hooks::{B0SafetyHook, Hook, HookCtx, PromptView};
    use tokio_util::sync::CancellationToken;

    let bot = MockBot::default();
    let fixture = include_bytes!("fixtures/sample.pdf");
    bot.stub_file_bytes("d3".into(), fixture.to_vec());

    let update = MockUpdate {
        id: 61,
        chat_id: 100,
        is_private: true,
        text: None,
        voice_file_id: None,
        document_file_id: Some("d3".into()),
        photo_file_id: None,
        caption: Some("file".into()),
        file_size: Some(fixture.len() as u64),
    };
    let tmp = tempfile::tempdir().unwrap();
    let agent_home = tmp.path().to_path_buf();

    // Bridge stages the artifact + ledger entry under the shared
    // agent_home (in production this is the user-agent's home; the
    // bridge writes there because it shares the supervisor process).
    let bridge_deps = FilesDeps {
        agent_home: agent_home.clone(),
        mime: "application/pdf".into(),
    };
    handle_document_update(&bot, &update, &bridge_deps)
        .await
        .unwrap();

    // The pipeline stamps `turn_id = 0` so we read turn 0 here. The
    // production runtime's hook layer promotes the entry to the active
    // turn; for this test we mirror the read at turn 0.
    let hook = B0SafetyHook::new();
    let ctx = HookCtx::for_test_with_home(agent_home, 0);
    let view = PromptView::empty();
    let tok = CancellationToken::new();
    let patch = hook.on_prompt_submit(&ctx, &view, &tok).await.unwrap();

    assert_eq!(
        patch.wrap_untrusted.len(),
        1,
        "expected exactly one wrapped artifact"
    );
    assert_eq!(patch.wrap_untrusted[0].tag, "untrusted_pdf_text");
    assert!(patch.wrap_untrusted[0].content.starts_with("--- page"));
    assert!(
        patch
            .turn_flags
            .contains(&"after_untrusted_input".to_string()),
        "B0 should raise the after-untrusted-input turn flag"
    );
}

#[test]
fn fixture_pdf_exists_and_nonempty() {
    let bytes = include_bytes!("fixtures/sample.pdf");
    assert!(!bytes.is_empty());
    assert_eq!(&bytes[..4], b"%PDF");
}

#[test]
fn fixture_png_exists_and_nonempty() {
    let bytes = include_bytes!("fixtures/sample.png");
    assert!(!bytes.is_empty());
    // PNG magic: 89 50 4E 47 ...
    assert_eq!(&bytes[..4], &[0x89, 0x50, 0x4E, 0x47]);
}
