//! Track C3 — M-c3.3 macOS Services menu channel tests.
//!
//! The whole channel is gated `#[cfg(target_os = "macos")]` because it
//! depends on `objc2-app-kit` (NSPasteboard) and the Cocoa runtime.
//! Linux / Windows builds compile a no-op stub so the integration test
//! still links.
//!
//! - M-c3.3.1: type exists + module compiles when objc2 deps are in
//!   place.
//! - M-c3.3.2: `extract_payload_from_pasteboard` decodes text/url/image
//!   into a `SharePayload`.
//! - M-c3.3.3: `Info.plist` injection (covered in
//!   `mur-core/tests/agent_export_gui_nsservices.rs`).
//! - M-c3.3.4: `MockApp::invoke_services_selector` end-to-end via
//!   harness, bypasses NSApplication round-trip.

#![cfg(target_os = "macos")]

#[test]
fn services_module_compiles_on_macos() {
    // The type only exists on macOS; this test asserts the module is
    // wired up and the objc2 deps resolve. No runtime behavior yet —
    // pasteboard decoding lands in M-c3.3.2.
    let _ = std::any::TypeId::of::<mur_agent_gui_lib::send::services::ServicesProvider>();
}
