//! Track C3 channel C — macOS Services menu integration.
//!
//! The Services menu is the lowest-friction native share entry point on
//! macOS — Bear, Drafts, Things, Mail, Safari all expose selections and
//! links there. We register a [`ServicesProvider`] with
//! `NSApplication.servicesProvider` so when the user picks
//! `Send Selection to <Agent>` (the default menu item rendered via
//! `Info.plist`'s `NSServices` array, see M-c3.3.3), AppKit invokes our
//! `serviceShare:userData:error:` selector, which decodes
//! `NSPasteboard` and hands the resulting [`SharePayload`] to the
//! [`SendIngestor`].
//!
//! Production wiring (`NSApplication.setServicesProvider:` from
//! `lib.rs::setup`) lands in a follow-up; the harness drives the same
//! seam through `MockApp::invoke_services_selector` (M-c3.3.4) so we
//! can iterate on the pasteboard decoder without spinning up a real
//! Cocoa runtime per test.
//!
//! [`SendIngestor`]: super::SendIngestor
//! [`SharePayload`]: super::SharePayload

#![cfg(target_os = "macos")]

use objc2::rc::Retained;
use objc2::runtime::NSObject;

/// Owns the boxed Objective-C subclass instance registered with
/// `NSApplication.servicesProvider`. The actual selector body and
/// pasteboard decoder land in M-c3.3.2.
pub struct ServicesProvider {
    _obj: Retained<NSObject>,
}
