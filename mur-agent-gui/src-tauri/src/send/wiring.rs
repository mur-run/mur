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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_home_honors_mur_home_override() {
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
}
