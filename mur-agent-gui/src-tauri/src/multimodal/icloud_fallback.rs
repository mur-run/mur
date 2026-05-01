//! iCloud / Apple Photos lazy-load fallback (drag-drop step 2,
//! roadmap §4.3).
//!
//! Apple Photos delivers iCloud images via the clipboard image
//! buffer, not the filesystem — Tauri's `on_drag_drop_event` fires
//! with an EMPTY `paths` list in that case. Fall back to reading the
//! clipboard image when paths is empty.
//!
//! The real Tauri-clipboard wiring (`tauri_plugin_clipboard_manager`)
//! lives in `commands.rs` where the `AppHandle` is in scope; this
//! module provides the trait abstraction so unit tests can inject a
//! fake clipboard.

/// Trait abstraction so tests can inject a fake clipboard. The
/// production impl wraps `tauri_plugin_clipboard_manager::ClipboardExt`
/// and lives in `commands.rs` (M3.6).
pub trait ClipboardSource {
    fn read_image(&self) -> Option<Vec<u8>>;
}

/// Returns the clipboard image bytes when available, else None.
/// Callers use this when `WebviewWindow::on_drag_drop_event` fires
/// with an empty `paths` Vec (the iCloud lazy-load case).
pub fn icloud_fallback_bytes(clip: &dyn ClipboardSource) -> Option<Vec<u8>> {
    clip.read_image()
}
