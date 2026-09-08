//! Authorization surface for the UNUserNotificationCenter backend (issue #1135).
//!
//! `NSUserNotification` needed none of this: it posted, and the app appeared in
//! Notifications settings afterwards. UN will not post until the user has been
//! asked, and the answer can be **no** — at the prompt, or later in System
//! Settings. That is the failure mode the migration buys, so it has to be
//! visible rather than presenting as notifications that quietly stop.
//!
//! Two entry points, both no-ops when the backend is not compiled in, so the
//! call sites carry no `cfg`:
//!
//! - [`request_authorization_once`] at startup — macOS prompts the first time
//!   and replays the stored answer afterwards, so this is safe to call on
//!   every launch.
//! - [`authorization_note`] when a send fails — turns "could not show
//!   notification" into a line that names the permission, which is the whole
//!   complaint behind the sibling MCP issue (#1161) in a different subsystem.

#[cfg(all(target_os = "macos", feature = "macos-un-notifications"))]
mod imp {
    /// Ask macOS for permission to post, once per launch, off the main thread.
    ///
    /// Spawned rather than awaited: Tauri's `setup` runs on the thread that is
    /// about to drive the UI, and the authorization completion handler fires on
    /// an Apple-internal queue, so blocking here would stall the window for no
    /// gain. Nothing downstream needs the answer synchronously — a send that
    /// races the prompt fails and is retried on the next inbox sweep.
    pub fn request_authorization_once() {
        std::thread::spawn(|| match notify_rust::request_auth_blocking() {
            Ok(true) => tracing::info!("macOS notification authorization granted"),
            // Not an error: the user is allowed to say no. Say it once, at warn,
            // so a later "notifications never arrive" report has a first line to
            // search for.
            Ok(false) => tracing::warn!(
                "macOS notification authorization DENIED — the Hub cannot post \
                 notifications until it is allowed in System Settings › \
                 Notifications › MUR Hub"
            ),
            Err(e) => tracing::warn!("could not request macOS notification authorization: {e}"),
        });
    }

    /// A human-readable reason a notification may not have been delivered, or
    /// `None` when authorization is not the explanation.
    ///
    /// This is the detection point issue #1135 asked for. `show()` returning
    /// `Ok` never meant "the user saw it", and on the old backend there was no
    /// API that could tell us otherwise; UN reports the real state, so a failed
    /// send can finally name a permission instead of a bare error.
    pub fn authorization_note() -> Option<String> {
        match notify_rust::get_notification_settings_blocking() {
            Ok(s) => {
                // Debug string rather than the enum: notify-rust re-exports the
                // functions but not `AuthorizationStatus`, and mac-usernotifications
                // is not a direct dependency, so the type cannot be named here.
                let status = format!("{:?}", s.authorization_status);
                // Only the two states that actually stop delivery. Provisional
                // and Ephemeral both post — quoting them would blame the
                // permission for a failure it did not cause.
                let blocking = status.contains("Denied") || status.contains("NotDetermined");
                blocking.then(|| {
                    format!(
                        "macOS notification authorization is {status} — allow MUR Hub in \
                         System Settings › Notifications"
                    )
                })
            }
            Err(e) => Some(format!("could not read macOS notification settings: {e}")),
        }
    }
}

#[cfg(not(all(target_os = "macos", feature = "macos-un-notifications")))]
mod imp {
    /// No-op: the deprecated `NSUserNotification` backend has no authorization
    /// step, and neither do Linux or Windows.
    pub fn request_authorization_once() {}

    /// No-op: without UN there is no API that can report why a post failed.
    pub fn authorization_note() -> Option<String> {
        None
    }
}

pub use imp::{authorization_note, request_authorization_once};
