//! Track C3 channel C — macOS Services menu integration.
//!
//! The Services menu is the lowest-friction native share entry point on
//! macOS — Bear, Drafts, Things, Mail, Safari all expose selections and
//! links there. Production (deferred to a `lib.rs::setup` follow-up)
//! will register a [`ServicesProvider`] with
//! `NSApplication.servicesProvider`. When the user picks
//! `Send Selection to <Agent>` (the menu item rendered via `Info.plist`'s
//! `NSServices` array — see M-c3.3.3), AppKit invokes the
//! `serviceShare:userData:error:` selector on our subclass, which calls
//! [`extract_payload_from_pasteboard`] and hands the resulting
//! [`SharePayload`] to the [`SendIngestor`].
//!
//! This module exposes the pasteboard decoder as a free function so it
//! can be exercised without spinning up a real Cocoa runtime per test.
//! The harness drives the same seam through
//! `MockApp::invoke_services_selector` (M-c3.3.4).
//!
//! [`SendIngestor`]: super::SendIngestor

#![cfg(target_os = "macos")]

use anyhow::bail;
use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypePNG, NSPasteboardTypeString};
use objc2_foundation::NSString;

use super::{ShareKind, SharePayload};

/// Owns the boxed Objective-C subclass instance registered with
/// `NSApplication.servicesProvider`. The selector body lives in the
/// production wiring step (lib.rs::setup); the unit-testable
/// pasteboard decoder is [`extract_payload_from_pasteboard`].
pub struct ServicesProvider {
    _obj: Retained<NSObject>,
}

/// Decode an `NSPasteboard` into a [`SharePayload`].
///
/// Routing rules mirror the hotkey channel ([`super::hotkey::synthesize_from_clipboard`]):
/// 1. `NSPasteboardTypeString` text starting with `http(s)://`
///    → [`ShareKind::Url`].
/// 2. Other text → [`ShareKind::Text`].
/// 3. `NSPasteboardTypePNG` data → write to a persisted temp PNG,
///    return [`ShareKind::Image`] pointing at it.
/// 4. Pasteboard offers neither → fail with a clear error so the
///    selector body can return an `NSError` and AppKit shows the
///    standard "couldn't share" alert.
pub fn extract_payload_from_pasteboard(pb: &NSPasteboard) -> anyhow::Result<SharePayload> {
    if let Some(text) = read_string(pb)
        && !text.is_empty()
    {
        let kind = if text.starts_with("http://") || text.starts_with("https://") {
            ShareKind::Url(text)
        } else {
            ShareKind::Text(text)
        };
        return Ok(SharePayload {
            source: "services".into(),
            kind,
            metadata: serde_json::json!({}),
        });
    }
    if let Some(bytes) = read_png(pb)
        && !bytes.is_empty()
    {
        let mut named = tempfile::Builder::new()
            .prefix("mur-share-")
            .suffix(".png")
            .tempfile()?;
        std::io::Write::write_all(named.as_file_mut(), &bytes)?;
        let (_, path) = named.keep()?;
        return Ok(SharePayload {
            source: "services".into(),
            kind: ShareKind::Image(path),
            metadata: serde_json::json!({}),
        });
    }
    bail!("pasteboard offers neither text nor PNG image — nothing to share")
}

fn read_string(pb: &NSPasteboard) -> Option<String> {
    unsafe { pb.stringForType(NSPasteboardTypeString) }.map(|ns| ns.to_string())
}

fn read_png(pb: &NSPasteboard) -> Option<Vec<u8>> {
    unsafe { pb.dataForType(NSPasteboardTypePNG) }.map(|data| data.to_vec())
}

/// Test helpers — exposed under `#[doc(hidden)]` so integration tests
/// can build private `NSPasteboard` instances without polluting the
/// user's actual general pasteboard. Production callers must not
/// reach for these.
#[doc(hidden)]
pub mod test_helpers {
    use super::*;
    use objc2::Message;
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::{NSArray, NSData};

    /// Build a `pasteboardWithUniqueName` carrying the given UTF-8 text
    /// under `NSPasteboardTypeString`.
    pub fn pasteboard_with_text(text: &str) -> Retained<NSPasteboard> {
        let pb = NSPasteboard::pasteboardWithUniqueName();
        let types = NSArray::from_retained_slice(&[unsafe { NSPasteboardTypeString.retain() }]);
        unsafe { pb.declareTypes_owner(&types, None) };
        let ns = NSString::from_str(text);
        unsafe {
            pb.setString_forType(&ns, NSPasteboardTypeString);
        }
        pb
    }

    /// Build an empty `pasteboardWithUniqueName` (no declared types).
    /// Used by tests to assert the decoder errors when both text and
    /// PNG slots are empty.
    pub fn empty_pasteboard() -> Retained<NSPasteboard> {
        NSPasteboard::pasteboardWithUniqueName()
    }

    /// Build a `pasteboardWithUniqueName` carrying PNG bytes under
    /// `NSPasteboardTypePNG`.
    pub fn pasteboard_with_image(bytes: &[u8]) -> Retained<NSPasteboard> {
        let pb = NSPasteboard::pasteboardWithUniqueName();
        let types = NSArray::from_retained_slice(&[unsafe { NSPasteboardTypePNG.retain() }]);
        unsafe { pb.declareTypes_owner(&types, None) };
        let data = NSData::with_bytes(bytes);
        unsafe {
            pb.setData_forType(Some(&data), NSPasteboardTypePNG);
        }
        pb
    }
}
