//! `mur agent companion connector ...` — bridge agent scaffolding (Track C1).
//!
//! In Track C1 only `--platform stub` is supported. The stub is a fully
//! functional A2A bridge agent (LLM disabled, identity keypair, default
//! route) that downstream tracks (C2 Telegram, C3 send-from-any-app)
//! specialise.

use anyhow::{Result, bail};

/// Scaffold a new bridge agent. Currently only `--platform stub` is supported.
pub async fn add(name: String, platform: &str, default_route: &str) -> Result<()> {
    if platform != "stub" {
        bail!(
            "platform '{platform}' not supported in Track C1 — only 'stub' is available. \
             Telegram lands in C2; send-from-any-app in C3."
        );
    }
    if default_route.trim().is_empty() {
        bail!("--default-route must be non-empty");
    }
    scaffold_stub_bridge(&name, default_route).await
}

pub(crate) async fn scaffold_stub_bridge(_name: &str, _default_route: &str) -> Result<()> {
    bail!("scaffold not yet implemented") // M-c1.6.2
}
