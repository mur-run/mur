//! Track C3 — M-c3.2 global hotkey channel tests.
//!
//! Layered like the URL scheme suite:
//! - M-c3.2.1: pure `default_combo_for(slug)` derives a per-agent
//!   `CommandOrControl+Shift+M+<FIRST_LETTER>` combo.
//! - M-c3.2.2: `Clipboard` trait + `synthesize_from_clipboard`
//!   reads text/url/image and produces a `SharePayload`.
//! - M-c3.2.3: `resolve_combo(slug, user_override)` lets the user
//!   override the per-agent default in companion settings.
//! - M-c3.2.4: end-to-end via `MockApp::trigger_shortcut` — a hotkey
//!   firing reaches the ingestor with the clipboard contents.

use mur_agent_gui_lib::send::hotkey::default_combo_for;

#[test]
fn default_hotkey_combo_for_slug() {
    assert_eq!(default_combo_for("coach"), "CommandOrControl+Shift+M+C");
    assert_eq!(default_combo_for("draft"), "CommandOrControl+Shift+M+D");
}

#[test]
fn collision_when_two_agents_share_first_letter() {
    // Documented limitation: two agents starting with "c" will collide
    // on the default combo. The user has to override one of them via
    // the companion settings escape hatch — see M-c3.2.3.
    assert_eq!(default_combo_for("coach"), default_combo_for("creator"));
}
