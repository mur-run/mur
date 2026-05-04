//! M-c3.0.3 + M-c3.0.4: Track C3 share content wraps as
//! `<untrusted_share>` and triggers Rule 4 same-turn cooldown.
//!
//! Mirrors the structure of `b0_untrusted_wrapper.rs` (the M3.8.1 PDF
//! wrapper test) so the share path stays a peer of the existing PDF
//! and image text wrappers.

use mur_agent_runtime::hooks::{B0SafetyHook, Hook, HookCtx, PromptView};
use mur_agent_runtime::multimodal::pipeline::process_share_text;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn share_marker_gets_wrapped_with_untrusted_share_tag() {
    let tmp = TempDir::new().unwrap();
    let agent_home = tmp.path();

    // Stage a share entry through the same path the GUI ingestor uses
    // (process_share_text writes the sidecar with a `--- share` prefix
    // and appends a turn_id=0 ledger entry).
    process_share_text("https://attacker.example/foo", "url_scheme", agent_home).unwrap();

    // The runtime promotes ledger entries into the current turn before
    // reading; for tests we just hand the hook the same turn_id the
    // ledger was written with (0).
    let hook = B0SafetyHook::new();
    let ctx = HookCtx::for_test_with_home(agent_home.to_path_buf(), 0);
    let view = PromptView::empty();
    let tok = CancellationToken::new();
    let patch = hook.on_prompt_submit(&ctx, &view, &tok).await.unwrap();

    assert_eq!(patch.wrap_untrusted.len(), 1);
    let w = &patch.wrap_untrusted[0];
    assert_eq!(w.tag, "untrusted_share");
    assert_eq!(w.source, "share:url_scheme");
    // Body still carries the marker (consistent with the PDF path,
    // which leaves "--- page 1 ---" in the body); the tag dispatch
    // happens off `starts_with`.
    assert!(
        w.content.starts_with("--- share\n"),
        "wrapped body should retain the share marker: {:?}",
        w.content
    );
    assert!(w.content.contains("https://attacker.example/foo"));

    // Rule 4 cooldown flag must be set.
    assert!(
        patch
            .turn_flags
            .contains(&"after_untrusted_input".to_string()),
        "share content must set the after_untrusted_input flag; got {:?}",
        patch.turn_flags
    );
}
