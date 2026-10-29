//! The startup override's native confirmation (Hub spec §Command step 8,
//! §Quit, revision 10). Shown from Rust so the webview cannot answer it.
//!
//! It calls `rfd` directly, not `tauri-plugin-dialog`: the plugin 2.7.1
//! rewrites `rfd`'s `Cancel` into the second custom label
//! (`src/desktop.rs:236-239`), and the accept label is second so Return
//! cancels. Through the plugin, Escape or closing the dialog would accept.
//! `rfd` reports every outcome that is not a button click as `Cancel`, so
//! the one rule here — only `Custom(accept)` accepts — fails closed.
//!
//! `HubConfirmation` is the only way this crate builds `Issuer::Hub`; a
//! source scan in `guard_tests.rs` keeps it that way.

use mur_common::agent::OverrideFlags;
use mur_core::cmd::agent::{Issuer, banner};
use rfd::{AsyncMessageDialog, MessageButtons, MessageDialogResult, MessageLevel};
use tauri::{AppHandle, WebviewWindow};
use tracing::warn;

pub const CANCEL_LABEL: &str = "Cancel";
pub const ACCEPT_LABEL: &str = "Restart with overrides";
pub const QUIT_LABEL: &str = "Quit anyway";

const OVERRIDE_TITLE: &str = "Startup override";

/// Proof that a person accepted the native dialog for these flags in this
/// window. Constructed only by [`confirm_override`].
#[derive(Debug)]
pub struct HubConfirmation {
    window_label: String,
    flags: OverrideFlags,
}

impl HubConfirmation {
    pub fn flags(&self) -> OverrideFlags {
        self.flags
    }

    pub fn issuer(&self) -> Issuer {
        Issuer::Hub {
            window_label: self.window_label.clone(),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(window: &str, flags: OverrideFlags) -> Self {
        Self {
            window_label: window.to_owned(),
            flags,
        }
    }
}

/// Only a click on the `accept` button is acceptance. Closing, Escape,
/// Return (the Cancel default), `Ok`, `Cancel`, any other label: declined.
pub fn accepted(result: &MessageDialogResult, accept: &str) -> bool {
    matches!(result, MessageDialogResult::Custom(label) if label == accept)
}

/// Cancel first: on macOS the first button is the default, so Return cancels.
fn buttons(accept: &str) -> MessageButtons {
    MessageButtons::OkCancelCustom(CANCEL_LABEL.to_owned(), accept.to_owned())
}

/// The dialog says exactly what the CLI's TTY confirmation says.
fn body(name: &str, flags: OverrideFlags) -> String {
    banner(name, flags)
}

/// Show a warning dialog modal to `parent` and block until it closes.
/// Call from a blocking thread, never the main thread: the dialog is built on
/// the main thread (as the plugin does) and awaited here. Any failure on the
/// way counts as declined.
pub fn ask(
    app: &AppHandle,
    parent: Option<&WebviewWindow>,
    title: &str,
    body: &str,
    accept: &str,
) -> bool {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let parent = parent.cloned();
    let (title, body, accept_owned) = (title.to_owned(), body.to_owned(), accept.to_owned());
    let scheduled = app.run_on_main_thread(move || {
        let mut d = AsyncMessageDialog::new()
            .set_level(MessageLevel::Warning)
            .set_title(title)
            .set_description(body)
            .set_buttons(buttons(&accept_owned));
        if let Some(w) = &parent {
            d = d.set_parent(w);
        }
        let _ = tx.send(d.show());
    });
    if let Err(e) = scheduled {
        warn!("could not show the override dialog: {e}");
        return false;
    }
    let Ok(fut) = rx.recv() else {
        warn!("the override dialog was dropped before it showed");
        return false;
    };
    let result = tauri::async_runtime::block_on(fut);
    accepted(&result, accept)
}

/// Ask the person at `dashboard` to confirm `flags` for `name`.
pub fn confirm_override(
    app: &AppHandle,
    dashboard: &WebviewWindow,
    name: &str,
    flags: OverrideFlags,
) -> Option<HubConfirmation> {
    ask(
        app,
        Some(dashboard),
        OVERRIDE_TITLE,
        &body(name, flags),
        ACCEPT_LABEL,
    )
    .then(|| HubConfirmation {
        window_label: dashboard.label().to_owned(),
        flags,
    })
}

#[cfg(test)]
#[path = "dialog_tests.rs"]
mod tests;
