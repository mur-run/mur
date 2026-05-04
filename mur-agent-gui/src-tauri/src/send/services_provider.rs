//! Track C3 / M-w3b — Services-menu runtime registration.
//!
//! Declares the Objective-C subclass that AppKit hands the
//! `serviceShare:` invocation to when the user picks
//! `Send Selection / Link / Image to <Agent>` from any app's
//! Services submenu. The `NSServices` Info.plist entries (rendered
//! per-agent by `agent_export_gui::rewrite_nsservices` in M-w3) tell
//! macOS which selector to dispatch and which menu titles to show;
//! this module provides the selector body.
//!
//! Architecture:
//! 1. [`MurServicesProvider`] — `NSObject` subclass holding an
//!    `Arc<dyn SendIngestor>` ivar so the selector body can dispatch
//!    payloads without capturing a Rust closure (Cocoa selectors
//!    can't carry Rust closures).
//! 2. `serviceShare:userData:error:` — selector body. Decodes the
//!    pasteboard via [`super::services::extract_payload_from_pasteboard`]
//!    and hands the resulting [`SharePayload`] to the ingestor on
//!    `tauri::async_runtime::spawn`.
//! 3. [`register_services_provider`] — call from `setup` to build a
//!    provider instance and register it with
//!    `NSApplication.sharedApplication.setServicesProvider:`.
//!
//! Production-only path: the harness (`MockApp::invoke_services_selector`)
//! drives the same seam through `extract_payload_from_pasteboard`
//! directly, so this module isn't exercised by `cargo test`.

#![cfg(target_os = "macos")]

use std::sync::Arc;

use anyhow::{Context, Result};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSPasteboard};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSString};

use super::SendIngestor;
use super::services::extract_payload_from_pasteboard;

/// Instance variables for the provider. Holding the ingestor as an
/// ivar keeps the registration self-contained — no globals, no
/// `OnceLock` — and means multiple providers (one per agent) can
/// coexist if the host ever runs more than one agent in-process.
pub struct ServicesIvars {
    ingestor: Arc<dyn SendIngestor>,
}

define_class!(
    // SAFETY:
    // - Superclass `NSObject` has no subclassing requirements.
    // - `MurServicesProvider` does not implement `Drop`. The ingestor
    //   `Arc` is dropped via the auto-derived ivar drop glue, which
    //   is sound for `Send + Sync` types.
    #[unsafe(super = NSObject)]
    #[thread_kind = objc2::MainThreadOnly]
    #[ivars = ServicesIvars]
    pub struct MurServicesProvider;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for MurServicesProvider {}

    impl MurServicesProvider {
        // SAFETY: signature matches Apple's
        // `- (void)serviceShare:(NSPasteboard *)pboard
        //                userData:(NSString *)userData
        //                   error:(NSString **)error;`
        // `error` is the standard NSString out-pointer; production
        // never writes through it (failures log + AppKit shows the
        // standard "couldn't share" banner from the lack of bytes).
        #[unsafe(method(serviceShare:userData:error:))]
        fn service_share(
            &self,
            pb: &NSPasteboard,
            _user_data: Option<&NSString>,
            _error: *mut *mut NSString,
        ) {
            match extract_payload_from_pasteboard(pb) {
                Ok(payload) => {
                    let ingestor = Arc::clone(&self.ivars().ingestor);
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = ingestor.ingest(payload).await {
                            tracing::warn!(error = %e, "services-menu ingest failed");
                        }
                    });
                }
                Err(e) => {
                    // Pasteboard didn't carry text or PNG. Log at
                    // debug — the user picked the menu but the
                    // pasteboard had nothing for us; not an error.
                    tracing::debug!(
                        error = %e,
                        "services-menu invocation: pasteboard had nothing to share"
                    );
                }
            }
        }
    }
);

impl MurServicesProvider {
    fn new(mtm: MainThreadMarker, ingestor: Arc<dyn SendIngestor>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ServicesIvars { ingestor });
        // SAFETY: `NSObject`'s `init` returns Self with no extra
        // safety requirements; ivars were set above.
        unsafe { msg_send![super(this), init] }
    }
}

/// `Send + Sync` wrapper that keeps a [`MurServicesProvider`] alive
/// for the lifetime of the app.
///
/// Tauri's `Manager::manage<T>` requires `T: Send + Sync + 'static`.
/// `Retained<MurServicesProvider>` is neither (the underlying
/// `MainThreadOnly` Cocoa object can only be touched from the main
/// thread). The wrapper exists purely as a strong-reference
/// container — we never call any Cocoa methods through it from app
/// state — so manually asserting `Send + Sync` is sound: the only
/// effect of the held value is its drop, which deregisters the
/// provider; that drop runs on whatever thread Tauri tears state
/// down on, but `Retained` drop is just `release` and `release` is
/// safe from any thread for objects that don't override `dealloc`
/// with main-thread-only logic. `MurServicesProvider`'s storage
/// (an `Arc<dyn SendIngestor>`) is itself `Send + Sync`, and
/// `NSObject::dealloc` (the only inherited dealloc) is thread-safe.
pub struct SharedServicesProvider(#[allow(dead_code)] Retained<MurServicesProvider>);

// SAFETY: see struct doc comment.
unsafe impl Send for SharedServicesProvider {}
// SAFETY: see struct doc comment.
unsafe impl Sync for SharedServicesProvider {}

impl SharedServicesProvider {
    pub fn new(provider: Retained<MurServicesProvider>) -> Self {
        Self(provider)
    }
}

/// Registers a [`MurServicesProvider`] with the running
/// `NSApplication`. Must be called from the main thread (Tauri's
/// `setup` runs there). Returns the retained provider so the caller
/// can stash it in app state — `NSApplication.servicesProvider:`
/// holds a weak reference, so dropping the `Retained` deregisters
/// us silently.
///
/// `MainThreadMarker::new()` returns `None` if we're not on the main
/// thread. In production we always are; the bail message exists so a
/// future test runner that calls this from a worker thread fails
/// loudly instead of running afoul of Cocoa thread-safety.
pub fn register_services_provider(
    ingestor: Arc<dyn SendIngestor>,
) -> Result<Retained<MurServicesProvider>> {
    let mtm = MainThreadMarker::new()
        .context("register_services_provider must run on the main thread")?;
    let provider = MurServicesProvider::new(mtm, ingestor);
    let app = NSApplication::sharedApplication(mtm);
    let any: &AnyObject = &provider;
    // SAFETY: `setServicesProvider:` accepts any object that
    // implements the `serviceShare:` selector; we declared it in
    // `define_class!` above, so the selector dispatch table is
    // populated.
    unsafe { app.setServicesProvider(Some(any)) };
    Ok(provider)
}
