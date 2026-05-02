//! Bridge event types + inbox markdown parser.
//!
//! `BridgeEvent` is the common over-the-wire shape between the Rust
//! bridge and the React UI. It is `serde::Serialize` so it can ride
//! a Tauri 2 `Channel<BridgeEvent>`.

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeEvent {
    pub id: String,
    pub situation: String,
    pub template_id: String,
    pub locale: String,
    pub generated_at: String,
    pub body: String,
    pub response: BridgeResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum BridgeResponse {
    Unset,
    Signal(String),
}

#[derive(Deserialize)]
struct FrontMatter {
    id: String,
    situation: String,
    template_id: String,
    locale: String,
    generated_at: String,
}

pub fn parse_inbox_md(path: &Path) -> Result<BridgeEvent> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse_str(&raw)
}

pub fn parse_str(raw: &str) -> Result<BridgeEvent> {
    // Expected layout (see mur-agent-runtime/src/companion/inbox.rs::render):
    //   ---
    //   <yaml front-matter>
    //   ---
    //
    //   <body>
    //
    //   >>> response: <unset>|good|bad|dismiss
    let stripped = raw
        .strip_prefix("---\n")
        .ok_or_else(|| anyhow!("missing opening front-matter fence"))?;
    let (yaml, rest) = stripped
        .split_once("\n---\n")
        .ok_or_else(|| anyhow!("missing closing front-matter fence"))?;
    let fm: FrontMatter = serde_yaml_ng::from_str(yaml).context("parse front-matter")?;

    // The body is everything after the closing fence up to the response line.
    let response_marker = ">>> response:";
    let (body_block, response_line) = rest
        .rsplit_once(response_marker)
        .ok_or_else(|| anyhow!("missing response marker"))?;
    let body = body_block.trim().to_string();
    let response_value = response_line.trim();
    let response = if response_value == "<unset>" {
        BridgeResponse::Unset
    } else if matches!(response_value, "good" | "bad" | "dismiss") {
        BridgeResponse::Signal(response_value.to_string())
    } else {
        bail!("unrecognized response value: {response_value}");
    };

    Ok(BridgeEvent {
        id: fm.id,
        situation: fm.situation,
        template_id: fm.template_id,
        locale: fm.locale,
        generated_at: fm.generated_at,
        body,
        response,
    })
}
