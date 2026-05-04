//! Track C3 production-wiring plumbing.
//!
//! The four channel modules (`url_scheme`, `hotkey`, `services`,
//! `dock`) ship pure parser/decoder/classifier surfaces plus harness
//! [`MockApp`] coverage. This module bridges them to the live Tauri
//! runtime: it owns the `AppHandle`-backed [`ShareEmitter`] that emits
//! `share:received` to the React composer, and it constructs the
//! production [`DefaultIngestor`] each channel callback feeds.
//!
//! Lives behind its own module so harness tests don't accidentally
//! pull in `tauri::AppHandle` (which would require the full Tauri
//! runtime + a frontend bundle to instantiate).
//!
//! [`MockApp`]: crate::test_harness::MockApp

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tauri::{AppHandle, Emitter};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use super::hotkey::{Clipboard, resolve_combo, synthesize_from_clipboard};
use super::{DefaultIngestor, SendIngestor, ShareEmitter, SharePayload};

/// Live emitter that pushes `share:received` events into the webview
/// so React's [`startShareListener`](../../../ui/src/lib/share.ts) can
/// flash the badge and insert the body.
///
/// Cloning is cheap — `AppHandle` is itself `Clone`-able and refcounted
/// internally — so callers can hand each channel callback its own
/// emitter without sharing a `Mutex`.
pub struct EventShareEmitter {
    app: AppHandle,
}

impl EventShareEmitter {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl ShareEmitter for EventShareEmitter {
    fn emit_received(&self, payload: &SharePayload) -> Result<()> {
        // Tauri's `emit` broadcasts to every webview; the composer
        // mounts a single listener so we don't worry about scoping
        // to a specific window. If multi-window scoping ever
        // matters, switch to `emit_to(...)` against the settings
        // window label.
        self.app
            .emit("share:received", payload)
            .with_context(|| "emit share:received event")
    }
}

/// Live [`Clipboard`] impl backed by `tauri-plugin-clipboard-manager`.
///
/// `read_text` returns `None` when the pasteboard has no text slot
/// (the plugin returns `Err`; we map that to `None` so the synthesizer
/// can short-circuit cleanly). `read_image` reads RGBA pixels from
/// the system clipboard and re-encodes them as PNG so the existing
/// `synthesize_from_clipboard` machinery can persist a temp file
/// and route it through the multimodal pipeline.
pub struct TauriClipboard {
    app: AppHandle,
}

impl TauriClipboard {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

#[async_trait::async_trait]
impl Clipboard for TauriClipboard {
    async fn read_text(&self) -> Result<Option<String>> {
        // The plugin returns Err when the pasteboard has no text
        // slot (vs. a slot containing an empty string). Both are
        // "nothing to share"; collapsing them keeps the synthesizer
        // contract uniform with `FakeClipboard`.
        Ok(self.app.clipboard().read_text().ok().filter(|s| !s.is_empty()))
    }

    async fn read_image(&self) -> Result<Option<Vec<u8>>> {
        let img = match self.app.clipboard().read_image() {
            Ok(img) => img,
            Err(_) => return Ok(None),
        };
        let rgba = img.rgba();
        let width = img.width();
        let height = img.height();
        if rgba.is_empty() || width == 0 || height == 0 {
            return Ok(None);
        }
        // Re-encode RGBA → PNG so downstream code that writes
        // `mur-share-*.png` files gets bytes the OS / image viewers
        // / mime-sniffer all recognise.
        let buf = image::RgbaImage::from_raw(width, height, rgba.to_vec())
            .context("clipboard rgba dimensions don't match buffer length")?;
        let mut png = Vec::with_capacity(rgba.len());
        image::DynamicImage::ImageRgba8(buf)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .context("encode clipboard image as PNG")?;
        Ok(Some(png))
    }
}

/// Resolve `~/.mur/agents/<slug>/` for the live agent. Mirrors the
/// resolution used by the runtime sidecar so artifacts written by the
/// ingestor land in the same tree.
///
/// `MUR_HOME` overrides the user's home directory — used by the e2e
/// runner so tests don't pollute `$HOME/.mur`. Bootstrap exports
/// `MUR_GUI_AGENT_NAME`; falling back to `template` matches the
/// development-build convention.
pub fn agent_home_for(slug: &str) -> PathBuf {
    let home = std::env::var_os("MUR_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_else(std::env::temp_dir);
    home.join(".mur").join("agents").join(slug)
}

/// Construct the production [`DefaultIngestor`] for the given agent.
/// Each channel callback (deep-link, hotkey, Services, dock) shares
/// one ingestor instance so provenance + B0 cooldown state stay
/// consistent across channels.
pub fn build_ingestor(app: AppHandle, slug: &str) -> Arc<dyn SendIngestor> {
    Arc::new(DefaultIngestor {
        agent_home: agent_home_for(slug),
        emitter: Arc::new(EventShareEmitter::new(app)),
    })
}

/// Read `MUR_GUI_AGENT_NAME` (set by `bootstrap::bootstrap_if_needed`)
/// or fall back to `template`. The slug is what every channel uses to
/// scope its registration: deep-link checks the URL scheme matches
/// `muragent-<slug>://`, hotkey appends the slug's first letter to the
/// default combo, etc.
pub fn current_agent_slug() -> String {
    std::env::var("MUR_GUI_AGENT_NAME").unwrap_or_else(|_| "template".to_string())
}

/// Read the user's `share.hotkey` override from
/// `~/.mur/agents/<slug>/companion/state.yaml` if present.
/// Returns `None` when the file doesn't exist, the file can't be
/// parsed, or the field isn't set — the caller falls back to the
/// per-agent default combo.
///
/// Failures here are deliberately silent: a malformed state.yaml
/// shouldn't crash the agent on boot, and the user can still trigger
/// the default combo while they fix the file. We log at `debug` so
/// operators can correlate against `RUST_LOG=mur_agent_gui_lib=debug`
/// if something looks off.
pub fn read_user_hotkey_override(slug: &str) -> Option<String> {
    let path = agent_home_for(slug)
        .join("companion")
        .join("state.yaml");
    let raw = std::fs::read_to_string(&path).ok()?;
    let parsed: serde_yaml_ng::Value = serde_yaml_ng::from_str(&raw)
        .map_err(|e| {
            tracing::debug!(error = %e, path = %path.display(), "ignore malformed state.yaml");
            e
        })
        .ok()?;
    parsed
        .get("share")?
        .get("hotkey")?
        .as_str()
        .map(str::to_string)
}

/// Wire the channel B (hotkey + clipboard) registration. Reads the
/// user override (if any), registers the resolved combo with the
/// global-shortcut plugin, and runs the synthesize → ingest pipeline
/// on each press.
///
/// Errors during initial registration bubble out so the caller can
/// log + continue startup — the agent stays useful via the other
/// three channels even if the OS refused to bind the combo (e.g.
/// another app already owns it).
pub fn register_share_hotkey(
    app: &AppHandle,
    slug: &str,
    ingestor: Arc<dyn SendIngestor>,
) -> Result<String> {
    let combo = resolve_combo(slug, read_user_hotkey_override(slug).as_deref());
    let parsed: Shortcut = combo
        .parse()
        .with_context(|| format!("parse hotkey combo `{combo}`"))?;

    let app_for_handler = app.clone();
    app.global_shortcut()
        .on_shortcut(parsed, move |_, _, event| {
            // Trigger on Pressed (key-down) only — global-shortcut
            // fires both Pressed + Released; the share path doesn't
            // care about hold-to-talk semantics.
            if event.state() != ShortcutState::Pressed {
                return;
            }
            let cb = TauriClipboard::new(app_for_handler.clone());
            let ing = ingestor.clone();
            tauri::async_runtime::spawn(async move {
                match synthesize_from_clipboard(&cb).await {
                    Ok(payload) => {
                        if let Err(e) = ing.ingest(payload).await {
                            tracing::warn!(error = %e, "hotkey ingest failed");
                        }
                    }
                    Err(e) => {
                        // Empty clipboard is the common case — debug
                        // not warn so users hammering the combo by
                        // accident don't fill the log.
                        tracing::debug!(error = %e, "hotkey synth (clipboard empty?)");
                    }
                }
            });
        })
        .map_err(|e| anyhow::anyhow!("register share hotkey {combo}: {e}"))
        .with_context(|| format!("register share hotkey {combo}"))?;

    Ok(combo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::hotkey::default_combo_for;
    use std::sync::Mutex;

    // Tests in this module mutate process-wide env vars (MUR_HOME,
    // MUR_GUI_AGENT_NAME). Vitest-style parallel test execution
    // causes spurious failures when one test clears MUR_HOME mid-
    // assertion in another. A single mutex serialises them.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn agent_home_honors_mur_home_override() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // SAFETY: tests share a process; serializing via a literal
        // env-var write is fine because integration tests in this
        // crate don't otherwise touch MUR_HOME.
        unsafe {
            std::env::set_var("MUR_HOME", "/tmp/mur-test-home");
        }
        let p = agent_home_for("coach");
        assert_eq!(p, PathBuf::from("/tmp/mur-test-home/.mur/agents/coach"));
        unsafe {
            std::env::remove_var("MUR_HOME");
        }
    }

    #[test]
    fn current_agent_slug_falls_back_to_template() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // Save and clear so the test is independent of bootstrap state.
        let prior = std::env::var("MUR_GUI_AGENT_NAME").ok();
        unsafe {
            std::env::remove_var("MUR_GUI_AGENT_NAME");
        }
        assert_eq!(current_agent_slug(), "template");
        if let Some(v) = prior {
            unsafe {
                std::env::set_var("MUR_GUI_AGENT_NAME", v);
            }
        }
    }

    #[test]
    fn user_hotkey_override_reads_share_hotkey_field() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let mur_root = tmp.path();
        unsafe {
            std::env::set_var("MUR_HOME", mur_root);
        }

        let companion_dir = mur_root
            .join(".mur")
            .join("agents")
            .join("override-bot")
            .join("companion");
        std::fs::create_dir_all(&companion_dir).unwrap();
        std::fs::write(
            companion_dir.join("state.yaml"),
            "share:\n  hotkey: CommandOrControl+Alt+K\n",
        )
        .unwrap();

        assert_eq!(
            read_user_hotkey_override("override-bot").as_deref(),
            Some("CommandOrControl+Alt+K"),
        );

        unsafe {
            std::env::remove_var("MUR_HOME");
        }
    }

    #[test]
    fn user_hotkey_override_returns_none_when_state_missing() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("MUR_HOME", tmp.path());
        }
        assert_eq!(read_user_hotkey_override("ghost"), None);
        unsafe {
            std::env::remove_var("MUR_HOME");
        }
    }

    #[test]
    fn user_hotkey_override_tolerates_malformed_yaml() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp
            .path()
            .join(".mur")
            .join("agents")
            .join("malformed")
            .join("companion");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("state.yaml"), "this: is: not: valid").unwrap();

        unsafe {
            std::env::set_var("MUR_HOME", tmp.path());
        }
        // Must not panic and must not throw — silent fallback is the
        // contract documented above.
        assert_eq!(read_user_hotkey_override("malformed"), None);
        unsafe {
            std::env::remove_var("MUR_HOME");
        }
    }

    #[test]
    fn default_combo_helper_matches_underlying() {
        assert_eq!(default_combo_for("coach"), "CommandOrControl+Shift+M+C");
    }
}
