//! Global PTT shortcut. Default `Cmd+Shift+'` on macOS,
//! `Ctrl+Shift+'` elsewhere. User-rebindable via Settings → Voice
//! → Hotkey (rebind UI lands in M1.6.3).
//!
//! Why not Fn: post-2021 Touch ID Macs route Fn through HIToolbox; it
//! cannot be registered via `RegisterEventHotKey`, so `tauri-plugin-
//! global-shortcut` can't see it without the Accessibility API
//! (which would require a TCC prompt).
//!
//! Default-off contract: hotkey is **not** registered at app startup.
//! `voice_enable` (commands.rs) calls `register_ptt`; `voice_disable`
//! calls `unregister_ptt`. On boot, the supervisor re-registers if the
//! persisted `voice_state.json` says the user had voice enabled
//! (mirrors macOS TCC's principle: revocation must outlive process
//! restart, but so must opt-in).

use anyhow::{Context, Result};
use tauri::{AppHandle, Emitter};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

pub fn default_ptt_shortcut() -> Shortcut {
    let mods = if cfg!(target_os = "macos") {
        Modifiers::SUPER | Modifiers::SHIFT
    } else {
        Modifiers::CONTROL | Modifiers::SHIFT
    };
    Shortcut::new(Some(mods), Code::Quote)
}

pub fn register_ptt(app: &AppHandle) -> Result<()> {
    let shortcut = default_ptt_shortcut();
    let app_clone = app.clone();
    app.global_shortcut()
        .on_shortcut(shortcut, move |_app, _shortcut, event| {
            let kind = match event.state() {
                ShortcutState::Pressed => "ptt://hotkey-down",
                ShortcutState::Released => "ptt://hotkey-up",
            };
            let _ = app_clone.emit(kind, ());
        })
        .map_err(|e| anyhow::anyhow!("register PTT shortcut: {e}"))
        .context("register_ptt")
}

/// Unregister the PTT shortcut. Called by `voice_disable` so that
/// disabled-state mur agents don't keep a global hotkey live.
pub fn unregister_ptt(app: &AppHandle) -> Result<()> {
    let shortcut = default_ptt_shortcut();
    app.global_shortcut()
        .unregister(shortcut)
        .map_err(|e| anyhow::anyhow!("unregister PTT shortcut: {e}"))
        .context("unregister_ptt")
}
