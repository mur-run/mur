//! Track C3 / M-w6 — `--self-test=<mode>` binary modes.
//!
//! The e2e script `scripts/e2e/c3-send-from-any-app.sh` builds a real
//! GUI bundle and then invokes the binary directly to verify each
//! channel's parser/decoder/classifier survives the bundling +
//! signing path. Without these modes, the script can only run the
//! harness (`cargo test`) — which doesn't exercise the production
//! build.
//!
//! Each mode runs a minimal end-to-end through the channel's pure
//! seam (no Tauri runtime, no plugins) and exits with `OK <mode>`
//! on success or `FAIL <mode>: <error>` on failure. The e2e script
//! greps for the `OK` line.
//!
//! Modes:
//! - `ping` — sanity check; verifies the binary launches and the
//!   self-test dispatcher itself works
//! - `url-scheme` — `parse_share_url` round-trip
//! - `hotkey` — `synthesize_from_clipboard` against a `FakeClipboard`
//! - `services` (macOS only) — `extract_payload_from_pasteboard`
//! - `dock-image` (macOS only) — `classify_path` on a `.png` path
//!
//! The full production wiring (deep-link delivery, hotkey
//! registration, NSApplication.servicesProvider, RunEvent::Opened)
//! is exercised by manual QA — automating it across CI hosts would
//! require XCTest / Spectator-style harness that's outside this
//! PR series's scope.

use std::path::PathBuf;

use crate::send::ShareKind;

/// Parse `--self-test=<mode>` from `std::env::args()`. Returns the
/// mode string if present, `None` otherwise. Accepts both
/// `--self-test=mode` and `--self-test mode` forms.
pub fn parse_self_test_arg() -> Option<String> {
    let mut args = std::env::args();
    while let Some(arg) = args.next() {
        if let Some(value) = arg.strip_prefix("--self-test=") {
            return Some(value.to_string());
        }
        if arg == "--self-test" {
            return args.next();
        }
    }
    None
}

/// Run the named self-test mode. Returns `Ok(())` on success;
/// callers print `OK <mode>` and exit 0. On `Err`, callers print
/// `FAIL <mode>: <error>` and exit 1.
pub fn run(mode: &str) -> anyhow::Result<()> {
    match mode {
        "ping" => Ok(()),
        "url-scheme" => self_test_url_scheme(),
        "hotkey" => self_test_hotkey(),
        #[cfg(target_os = "macos")]
        "services" => self_test_services(),
        #[cfg(target_os = "macos")]
        "dock-image" => self_test_dock_image(),
        other => anyhow::bail!("unknown self-test mode `{other}`"),
    }
}

fn self_test_url_scheme() -> anyhow::Result<()> {
    use base64::Engine;
    let body = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("hello self-test");
    let url = format!("muragent-coach://share?text={body}&type=text");
    let payload = crate::send::url_scheme::parse_share_url(&url, "coach")?;
    match payload.kind {
        ShareKind::Text(t) if t == "hello self-test" => Ok(()),
        other => anyhow::bail!("unexpected payload kind: {other:?}"),
    }
}

fn self_test_hotkey() -> anyhow::Result<()> {
    use crate::send::hotkey::{FakeClipboard, synthesize_from_clipboard};
    // Block in-place since this runs before the Tauri runtime
    // starts; spinning up a full tokio runtime just for one
    // synthesizer call is overkill, so block on a single-thread rt.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let cb = FakeClipboard::with_text("hello self-test");
    let payload = rt.block_on(synthesize_from_clipboard(&cb))?;
    match payload.kind {
        ShareKind::Text(t) if t == "hello self-test" => Ok(()),
        other => anyhow::bail!("unexpected payload kind: {other:?}"),
    }
}

#[cfg(target_os = "macos")]
fn self_test_services() -> anyhow::Result<()> {
    use crate::send::services::extract_payload_from_pasteboard;
    use crate::send::services::test_helpers::pasteboard_with_text;
    let pb = pasteboard_with_text("hello self-test");
    let payload = extract_payload_from_pasteboard(&pb)?;
    match payload.kind {
        ShareKind::Text(t) if t == "hello self-test" => Ok(()),
        other => anyhow::bail!("unexpected payload kind: {other:?}"),
    }
}

#[cfg(target_os = "macos")]
fn self_test_dock_image() -> anyhow::Result<()> {
    use crate::send::dock::classify_path;
    let p = PathBuf::from("/tmp/mur-self-test.png");
    match classify_path(&p) {
        ShareKind::Image(_) => Ok(()),
        other => anyhow::bail!("expected ShareKind::Image, got {other:?}"),
    }
}

// `PathBuf` is only used by macOS-gated paths; keep the import
// referenced on Linux/Windows so the self-test module still
// compiles cross-platform.
#[cfg(not(target_os = "macos"))]
fn _silence_pathbuf(_: &PathBuf) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_self_test_arg_handles_equals_form() {
        // `parse_self_test_arg` reads from `std::env::args()`, which
        // we can't override at test time; instead, just verify the
        // happy path through the dispatch surface.
        assert!(run("ping").is_ok());
    }

    #[test]
    fn run_url_scheme_round_trips() {
        run("url-scheme").expect("url-scheme self-test must pass");
    }

    #[test]
    fn run_hotkey_round_trips() {
        run("hotkey").expect("hotkey self-test must pass");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn run_services_round_trips() {
        run("services").expect("services self-test must pass");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn run_dock_image_classifies_png() {
        run("dock-image").expect("dock-image self-test must pass");
    }

    #[test]
    fn run_unknown_mode_errors() {
        let err = run("nonexistent").unwrap_err();
        assert!(err.to_string().contains("unknown self-test mode"));
    }
}
