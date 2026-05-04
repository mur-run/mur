//! Track C3 channel B — global hotkey capture.
//!
//! Bound at app startup via [`tauri-plugin-global-shortcut`]. When the
//! user hits the bound combo, we read the system clipboard
//! (`tauri-plugin-clipboard-manager`), synthesize a [`SharePayload`],
//! and hand it to the [`SendIngestor`].
//!
//! `lib.rs::setup` wiring is deferred to a follow-up so we can iterate
//! on the combo + clipboard logic in isolation; the [`crate::test_harness::MockApp`]
//! drives the same trigger → ingest path used by production.
//!
//! [`SendIngestor`]: super::SendIngestor

use std::sync::Mutex;

use anyhow::bail;

use crate::send::{ShareKind, SharePayload};

/// Default hotkey combo for an agent.
///
/// Single-agent installs land on `Cmd+Shift+M`; the per-agent suffix
/// (the first letter of the slug, uppercased) only matters when more
/// than one mur agent is installed. Two agents whose slugs share a
/// first letter still collide — see
/// [`resolve_combo`] for the user-override escape hatch.
///
/// `slug` is expected to be the kebab-case agent name produced by
/// `mur-core::cmd::agent_export_gui::sanitize_for_bundle_id`. An
/// empty slug falls back to `A` so the combo stays valid.
pub fn default_combo_for(slug: &str) -> String {
    let first = slug
        .chars()
        .next()
        .map(|c| c.to_ascii_uppercase())
        .unwrap_or('A');
    format!("CommandOrControl+Shift+M+{first}")
}

/// Read-only clipboard surface the hotkey handler depends on.
///
/// Production wiring delegates to `tauri-plugin-clipboard-manager`;
/// tests inject a [`FakeClipboard`] so they don't need a real
/// pasteboard or a Tauri runtime.
#[async_trait::async_trait]
pub trait Clipboard: Send + Sync {
    async fn read_text(&self) -> anyhow::Result<Option<String>>;
    async fn read_image(&self) -> anyhow::Result<Option<Vec<u8>>>;
}

/// Synthesize a [`SharePayload`] from the current clipboard contents.
///
/// Routing rules:
/// 1. Text starting with `http://` or `https://` → [`ShareKind::Url`].
/// 2. Other text → [`ShareKind::Text`].
/// 3. Image bytes → write to a temp PNG file, return [`ShareKind::Image`]
///    pointing at it. The temp file persists past this call so the
///    [`super::SendIngestor`] can read it; cleanup is deferred to the
///    OS temp-dir GC, which is fine for hotkey usage where individual
///    payloads are small (<10 MB) and rare.
/// 4. Empty clipboard → fail with a clear error so the hotkey handler
///    can flash a "nothing to share" toast instead of silently
///    sending a blank payload.
pub async fn synthesize_from_clipboard(cb: &dyn Clipboard) -> anyhow::Result<SharePayload> {
    if let Some(text) = cb.read_text().await?
        && !text.is_empty()
    {
        let kind = if text.starts_with("http://") || text.starts_with("https://") {
            ShareKind::Url(text)
        } else {
            ShareKind::Text(text)
        };
        return Ok(SharePayload {
            source: "hotkey".into(),
            kind,
            metadata: serde_json::json!({}),
        });
    }
    if let Some(bytes) = cb.read_image().await?
        && !bytes.is_empty()
    {
        let mut named = tempfile::Builder::new()
            .prefix("mur-share-")
            .suffix(".png")
            .tempfile()?;
        std::io::Write::write_all(named.as_file_mut(), &bytes)?;
        let (_, path) = named.keep()?;
        return Ok(SharePayload {
            source: "hotkey".into(),
            kind: ShareKind::Image(path),
            metadata: serde_json::json!({}),
        });
    }
    bail!("clipboard empty — nothing to share")
}

/// Resolve the hotkey combo for an agent, honouring an optional user
/// override (read from `~/.mur/agents/<name>/companion/state.yaml`
/// field `share.hotkey`).
///
/// `None` falls back to [`default_combo_for`]. The override is taken
/// verbatim — the GUI is expected to validate it before persisting.
pub fn resolve_combo(slug: &str, user_override: Option<&str>) -> String {
    user_override
        .map(str::to_string)
        .unwrap_or_else(|| default_combo_for(slug))
}

/// Test fake — wraps `Mutex<Option<…>>` slots so tests can pre-load
/// either a text or an image and pass it to `synthesize_from_clipboard`
/// without bringing the real pasteboard plugin.
pub struct FakeClipboard {
    text: Mutex<Option<String>>,
    image: Mutex<Option<Vec<u8>>>,
}

impl FakeClipboard {
    pub fn empty() -> Self {
        Self {
            text: Mutex::new(None),
            image: Mutex::new(None),
        }
    }

    pub fn with_text(text: impl Into<String>) -> Self {
        Self {
            text: Mutex::new(Some(text.into())),
            image: Mutex::new(None),
        }
    }

    pub fn with_image(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            text: Mutex::new(None),
            image: Mutex::new(Some(bytes.into())),
        }
    }

    /// Drop any cached payload. Mirrors a user clearing the clipboard
    /// between two hotkey presses; used by harness tests.
    pub fn clear(&self) {
        *self.text.lock().expect("FakeClipboard mutex poisoned") = None;
        *self.image.lock().expect("FakeClipboard mutex poisoned") = None;
    }

    /// Replace the held text. Used by `MockApp::set_clipboard_text`
    /// in M-c3.2.4.
    pub fn set_text(&self, text: impl Into<String>) {
        *self.text.lock().expect("FakeClipboard mutex poisoned") = Some(text.into());
        *self.image.lock().expect("FakeClipboard mutex poisoned") = None;
    }
}

#[async_trait::async_trait]
impl Clipboard for FakeClipboard {
    async fn read_text(&self) -> anyhow::Result<Option<String>> {
        Ok(self
            .text
            .lock()
            .expect("FakeClipboard mutex poisoned")
            .clone())
    }
    async fn read_image(&self) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(self
            .image
            .lock()
            .expect("FakeClipboard mutex poisoned")
            .clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_default_combo_for_slug() {
        assert_eq!(default_combo_for("coach"), "CommandOrControl+Shift+M+C");
        assert_eq!(default_combo_for("draft"), "CommandOrControl+Shift+M+D");
        assert_eq!(default_combo_for("mur-bot"), "CommandOrControl+Shift+M+M");
    }

    #[test]
    fn unit_default_combo_empty_slug_falls_back_to_a() {
        assert_eq!(default_combo_for(""), "CommandOrControl+Shift+M+A");
    }
}
